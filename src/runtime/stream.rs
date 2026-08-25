//! Bounded credit-based stream state shared by IPC, models, MCP and agents.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use thiserror::Error;

#[derive(Debug, Clone)]
pub struct StreamLimits {
    pub max_streams_per_app: usize,
    pub max_chunk_bytes: usize,
    pub max_buffered_bytes_per_stream: usize,
    pub max_credit_bytes: usize,
    pub idle_timeout: Duration,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_streams_per_app: 16,
            max_chunk_bytes: 64 * 1024,
            max_buffered_bytes_per_stream: 512 * 1024,
            max_credit_bytes: 1024 * 1024,
            idle_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamChunk {
    pub sequence: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTerminal {
    Completed,
    Failed { code: String, message: String },
    Cancelled { reason: String },
}

#[derive(Debug, Clone)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StreamError {
    #[error("stream id must not be empty")]
    EmptyId,
    #[error("stream {0:?} already exists")]
    Duplicate(String),
    #[error("stream {0:?} was not found")]
    NotFound(String),
    #[error("application stream limit reached")]
    AppLimit,
    #[error("stream is already terminal")]
    Terminal,
    #[error("stream chunk exceeds {limit} bytes")]
    ChunkTooLarge { limit: usize },
    #[error("stream has insufficient credit: needed {needed}, available {available}")]
    Backpressured { needed: usize, available: usize },
    #[error("stream buffer exceeds {limit} bytes")]
    BufferFull { limit: usize },
    #[error("credit grant must be greater than zero")]
    InvalidCredit,
}

#[derive(Debug)]
struct StreamState {
    app_id: String,
    next_sequence: u64,
    credit_bytes: usize,
    buffered_bytes: usize,
    chunks: VecDeque<StreamChunk>,
    terminal: Option<StreamTerminal>,
    cancellation: CancellationToken,
    last_activity: Instant,
}

#[derive(Debug, Clone)]
pub struct StreamManager {
    limits: StreamLimits,
    shared: Arc<StreamShared>,
}

#[derive(Debug)]
struct StreamShared {
    streams: Mutex<BTreeMap<String, StreamState>>,
    changed: Condvar,
}

impl StreamManager {
    pub fn new(limits: StreamLimits) -> Self {
        Self {
            limits,
            shared: Arc::new(StreamShared {
                streams: Mutex::new(BTreeMap::new()),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn open(&self, app_id: &str, stream_id: &str) -> Result<CancellationToken, StreamError> {
        if stream_id.trim().is_empty() {
            return Err(StreamError::EmptyId);
        }
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        if streams.contains_key(stream_id) {
            return Err(StreamError::Duplicate(stream_id.into()));
        }
        if streams
            .values()
            .filter(|stream| stream.app_id == app_id && stream.terminal.is_none())
            .count()
            >= self.limits.max_streams_per_app
        {
            return Err(StreamError::AppLimit);
        }
        let cancellation = CancellationToken(Arc::new(AtomicBool::new(false)));
        streams.insert(
            stream_id.into(),
            StreamState {
                app_id: app_id.into(),
                next_sequence: 0,
                credit_bytes: 0,
                buffered_bytes: 0,
                chunks: VecDeque::new(),
                terminal: None,
                cancellation: cancellation.clone(),
                last_activity: Instant::now(),
            },
        );
        self.shared.changed.notify_all();
        Ok(cancellation)
    }

    pub fn grant_credit(&self, stream_id: &str, bytes: usize) -> Result<usize, StreamError> {
        if bytes == 0 {
            return Err(StreamError::InvalidCredit);
        }
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let stream = streams
            .get_mut(stream_id)
            .ok_or_else(|| StreamError::NotFound(stream_id.into()))?;
        if stream.terminal.is_some() {
            return Err(StreamError::Terminal);
        }
        stream.credit_bytes = stream
            .credit_bytes
            .saturating_add(bytes)
            .min(self.limits.max_credit_bytes);
        stream.last_activity = Instant::now();
        Ok(stream.credit_bytes)
    }

    pub fn push(&self, stream_id: &str, data: Vec<u8>) -> Result<u64, StreamError> {
        if data.len() > self.limits.max_chunk_bytes {
            return Err(StreamError::ChunkTooLarge {
                limit: self.limits.max_chunk_bytes,
            });
        }
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let stream = streams
            .get_mut(stream_id)
            .ok_or_else(|| StreamError::NotFound(stream_id.into()))?;
        if stream.terminal.is_some() {
            return Err(StreamError::Terminal);
        }
        if data.len() > stream.credit_bytes {
            return Err(StreamError::Backpressured {
                needed: data.len(),
                available: stream.credit_bytes,
            });
        }
        if stream.buffered_bytes.saturating_add(data.len())
            > self.limits.max_buffered_bytes_per_stream
        {
            return Err(StreamError::BufferFull {
                limit: self.limits.max_buffered_bytes_per_stream,
            });
        }
        let sequence = stream.next_sequence;
        stream.next_sequence = stream.next_sequence.saturating_add(1);
        stream.credit_bytes -= data.len();
        stream.buffered_bytes += data.len();
        stream.chunks.push_back(StreamChunk { sequence, data });
        stream.last_activity = Instant::now();
        self.shared.changed.notify_all();
        Ok(sequence)
    }

    pub fn pop(&self, stream_id: &str) -> Result<Option<StreamChunk>, StreamError> {
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let stream = streams
            .get_mut(stream_id)
            .ok_or_else(|| StreamError::NotFound(stream_id.into()))?;
        let chunk = stream.chunks.pop_front();
        if let Some(chunk) = &chunk {
            stream.buffered_bytes = stream.buffered_bytes.saturating_sub(chunk.data.len());
        }
        stream.last_activity = Instant::now();
        Ok(chunk)
    }

    /// Wait until a chunk, terminal state, removal, or timeout is observable.
    /// This is the transport-facing alternative to tight `stream.read` polling.
    pub fn pop_wait(
        &self,
        stream_id: &str,
        timeout: Duration,
    ) -> Result<Option<StreamChunk>, StreamError> {
        let deadline = Instant::now() + timeout;
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        loop {
            let stream = streams
                .get_mut(stream_id)
                .ok_or_else(|| StreamError::NotFound(stream_id.into()))?;
            if let Some(chunk) = stream.chunks.pop_front() {
                stream.buffered_bytes = stream.buffered_bytes.saturating_sub(chunk.data.len());
                stream.last_activity = Instant::now();
                return Ok(Some(chunk));
            }
            if stream.terminal.is_some() || timeout.is_zero() {
                return Ok(None);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let (next, wait) = self
                .shared
                .changed
                .wait_timeout(streams, deadline.saturating_duration_since(now))
                .expect("stream manager lock poisoned while waiting");
            streams = next;
            if wait.timed_out() {
                return Ok(None);
            }
        }
    }

    pub fn finish(&self, stream_id: &str, terminal: StreamTerminal) -> Result<(), StreamError> {
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let stream = streams
            .get_mut(stream_id)
            .ok_or_else(|| StreamError::NotFound(stream_id.into()))?;
        if stream.terminal.is_some() {
            return Err(StreamError::Terminal);
        }
        if matches!(terminal, StreamTerminal::Cancelled { .. }) {
            stream.cancellation.0.store(true, Ordering::Release);
        }
        stream.terminal = Some(terminal);
        stream.last_activity = Instant::now();
        self.shared.changed.notify_all();
        Ok(())
    }

    pub fn cancel(&self, stream_id: &str, reason: impl Into<String>) -> Result<(), StreamError> {
        self.finish(
            stream_id,
            StreamTerminal::Cancelled {
                reason: reason.into(),
            },
        )
    }

    pub fn terminal(&self, stream_id: &str) -> Result<Option<StreamTerminal>, StreamError> {
        self.shared.streams
            .lock()
            .expect("stream manager lock poisoned")
            .get(stream_id)
            .map(|stream| stream.terminal.clone())
            .ok_or_else(|| StreamError::NotFound(stream_id.into()))
    }

    pub fn remove(&self, stream_id: &str) -> bool {
        let removed = self.shared.streams
            .lock()
            .expect("stream manager lock poisoned")
            .remove(stream_id)
            .is_some();
        if removed {
            self.shared.changed.notify_all();
        }
        removed
    }

    /// Cancel and remove every stream owned by an application. Used when a
    /// page session or application host shuts down so producers cannot outlive
    /// their security identity.
    pub fn close_app(&self, app_id: &str) -> Vec<String> {
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let ids: Vec<String> = streams
            .iter()
            .filter_map(|(id, stream)| (stream.app_id == app_id).then_some(id.clone()))
            .collect();
        for id in &ids {
            if let Some(stream) = streams.remove(id) {
                stream.cancellation.0.store(true, Ordering::Release);
            }
        }
        self.shared.changed.notify_all();
        ids
    }

    pub fn reap_idle(&self) -> Vec<String> {
        let now = Instant::now();
        let mut streams = self.shared.streams.lock().expect("stream manager lock poisoned");
        let expired: Vec<String> = streams
            .iter()
            .filter_map(|(id, stream)| {
                (now.duration_since(stream.last_activity) >= self.limits.idle_timeout)
                    .then_some(id.clone())
            })
            .collect();
        for id in &expired {
            if let Some(stream) = streams.remove(id) {
                stream.cancellation.0.store(true, Ordering::Release);
            }
        }
        if !expired.is_empty() {
            self.shared.changed.notify_all();
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> StreamManager {
        StreamManager::new(StreamLimits {
            max_streams_per_app: 2,
            max_chunk_bytes: 4,
            max_buffered_bytes_per_stream: 6,
            max_credit_bytes: 8,
            idle_timeout: Duration::from_millis(1),
        })
    }

    #[test]
    fn producer_cannot_push_without_consumer_credit() {
        let manager = manager();
        manager.open("app", "stream").unwrap();
        assert_eq!(
            manager.push("stream", vec![1]),
            Err(StreamError::Backpressured {
                needed: 1,
                available: 0
            })
        );
    }

    #[test]
    fn credit_is_consumed_and_sequences_are_monotonic() {
        let manager = manager();
        manager.open("app", "stream").unwrap();
        assert_eq!(manager.grant_credit("stream", 8).unwrap(), 8);
        assert_eq!(manager.push("stream", vec![1, 2]).unwrap(), 0);
        assert_eq!(manager.push("stream", vec![3, 4]).unwrap(), 1);
        assert_eq!(manager.pop("stream").unwrap().unwrap().sequence, 0);
        assert_eq!(manager.pop("stream").unwrap().unwrap().sequence, 1);
    }

    #[test]
    fn buffer_and_chunk_limits_are_independent() {
        let manager = manager();
        manager.open("app", "stream").unwrap();
        manager.grant_credit("stream", 8).unwrap();
        assert!(matches!(
            manager.push("stream", vec![0; 5]),
            Err(StreamError::ChunkTooLarge { .. })
        ));
        manager.push("stream", vec![0; 4]).unwrap();
        assert!(matches!(
            manager.push("stream", vec![0; 3]),
            Err(StreamError::BufferFull { .. })
        ));
    }

    #[test]
    fn cancellation_reaches_worker_token_and_terminal_is_once_only() {
        let manager = manager();
        let token = manager.open("app", "stream").unwrap();
        manager.cancel("stream", "user").unwrap();
        assert!(token.is_cancelled());
        assert_eq!(
            manager.finish("stream", StreamTerminal::Completed),
            Err(StreamError::Terminal)
        );
    }

    #[test]
    fn per_app_limit_does_not_block_another_app() {
        let manager = manager();
        manager.open("a", "a-1").unwrap();
        manager.open("a", "a-2").unwrap();
        assert!(matches!(
            manager.open("a", "a-3"),
            Err(StreamError::AppLimit)
        ));
        manager.open("b", "b-1").unwrap();
    }

    #[test]
    fn idle_reap_cancels_and_removes_stream() {
        let manager = manager();
        let token = manager.open("app", "stream").unwrap();
        std::thread::sleep(Duration::from_millis(3));
        assert_eq!(manager.reap_idle(), vec!["stream"]);
        assert!(token.is_cancelled());
        assert_eq!(
            manager.terminal("stream"),
            Err(StreamError::NotFound("stream".into()))
        );
    }

    #[test]
    fn close_app_only_cancels_streams_for_that_identity() {
        let manager = manager();
        let first = manager.open("a", "a-1").unwrap();
        let second = manager.open("b", "b-1").unwrap();
        assert_eq!(manager.close_app("a"), vec!["a-1"]);
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert!(manager.terminal("b-1").is_ok());
    }
}
