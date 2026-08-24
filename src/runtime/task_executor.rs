//! Small bounded worker pool for blocking host work.

use std::sync::{
    Arc, Mutex, OnceLock,
    mpsc::{SyncSender, TrySendError, sync_channel},
};

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("executor queue is full")]
    Full,
    #[error("executor has shut down")]
    Closed,
}

#[derive(Clone)]
pub struct TaskExecutor {
    sender: SyncSender<Job>,
}

impl TaskExecutor {
    pub fn new(name: &str, workers: usize, queue_capacity: usize) -> Self {
        assert!(workers > 0 && queue_capacity > 0);
        let (sender, receiver) = sync_channel::<Job>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("{name}-{index}"))
                .spawn(move || {
                    loop {
                        let job = receiver.lock().expect("executor receiver lock").recv();
                        match job {
                            Ok(job) => job(),
                            Err(_) => break,
                        }
                    }
                })
                .expect("spawn task executor worker");
        }
        Self { sender }
    }

    pub fn submit(&self, job: impl FnOnce() + Send + 'static) -> Result<(), SubmitError> {
        self.sender
            .try_send(Box::new(job))
            .map_err(|error| match error {
                TrySendError::Full(_) => SubmitError::Full,
                TrySendError::Disconnected(_) => SubmitError::Closed,
            })
    }
}

pub fn ipc_executor() -> &'static TaskExecutor {
    static EXECUTOR: OnceLock<TaskExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| TaskExecutor::new("alex-ipc", 4, 64))
}

pub fn update_executor() -> &'static TaskExecutor {
    static EXECUTOR: OnceLock<TaskExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| TaskExecutor::new("alex-update", 2, 32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn executor_runs_submitted_work() {
        let executor = TaskExecutor::new("test-executor", 1, 2);
        let (tx, rx) = channel();
        executor.submit(move || tx.send(42).unwrap()).unwrap();
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn executor_rejects_work_when_queue_is_saturated() {
        let executor = TaskExecutor::new("test-bounded", 1, 1);
        let (release_tx, release_rx) = channel::<()>();
        executor
            .submit(move || {
                let _ = release_rx.recv();
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        executor.submit(|| {}).unwrap();
        assert!(matches!(executor.submit(|| {}), Err(SubmitError::Full)));
        release_tx.send(()).unwrap();
    }
}
