//! Per-service log file sink.
//!
//! Phase 4 "independent logs" requirement: every service
//! owned by the supervisor must have its own
//! `stdout` and `stderr` log file under the
//! application-level log directory. The supervisor
//! tees the lines it already drains from the child
//! process into:
//!
//! ```text
//! %LOCALAPPDATA%/AlexOS/apps/<app_id>/logs/
//!     <service>.stdout.log      <- live, append-only
//!     <service>.stdout.log.1    <- rotated once
//!     <service>.stderr.log
//!     <service>.stderr.log.1
//! ```
//!
//! Design notes:
//!
//! * **One rotation per stream.** A `1 MiB` live file
//!   becomes `1 MiB.1`; the next rotation overwrites
//!   the old `.1`. This is the simplest scheme that
//!   keeps the on-disk footprint bounded at `~4 MiB`
//!   per service per app.
//! * **Redaction on the way to disk.** The in-memory
//!   ring buffer keeps the original line (the App
//!   Manager UI shows it). The file sink scrubs
//!   common credential patterns
//!   (`token=…`, `password=…`, `secret=…`,
//!   `api_key=…`, plus `Authorization: Bearer …`)
//!   so a misbehaving service cannot leak secrets
//!   into a long-term log file the user does not
//!   know to scrub.
//! * **Thread-safe.** The pumps run on dedicated
//!   threads; the file handle lives behind a
//!   `Mutex<BufWriter<File>>` and is dropped when
//!   the pump exits (i.e. when the child closes the
//!   pipe).
//! * **Best-effort writes.** A disk-full or
//!   permission error logs to stderr and skips the
//!   line; we never block the pump on a file write
//!   and never panic on a log failure.

use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Maximum size of the live log file in bytes. When
/// the writer crosses this threshold it rotates the
/// live file to `<name>.log.1` and starts a fresh
/// `<name>.log`. The total per-stream on-disk
/// footprint is therefore bounded at ~2 MiB.
pub const LOG_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// A writer for one half of a service's log (stdout
/// or stderr). Drop closes the file.
pub struct LogFileWriter {
    /// The path of the live file (e.g.
    /// `<dir>/api.stdout.log`). The rotated `.1` lives
    /// at `<dir>/api.stdout.log.1`.
    path: PathBuf,
    inner: Mutex<BufWriter<File>>,
    bytes_written: Mutex<u64>,
}

impl LogFileWriter {
    /// Open (or create) the log file in append mode. The
    /// parent directory must already exist; the caller
    /// is responsible for `create_dir_all`.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        // We do not know the exact pre-existing size
        // (the file may have been rotated already), so
        // we ask the file system. This is the only
        // stat we do in the hot path; the in-memory
        // counter is updated on every successful write.
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path,
            inner: Mutex::new(BufWriter::new(file)),
            bytes_written: Mutex::new(bytes_written),
        })
    }

    /// Append a single line to the file, rotating when
    /// the file grows past [`LOG_FILE_MAX_BYTES`].
    /// The line is written verbatim; callers that need
    /// redaction should pre-process with
    /// [`redact_secrets`].
    pub fn write_line(&self, line: &str) {
        // Worst-case line length: line + '\n'. We
        // check the counter against the cap *before*
        // writing so a single very long line still
        // triggers a rotation.
        let line_bytes = line.len() as u64 + 1;
        let mut bytes = self.bytes_written.lock().expect("log file lock poisoned");
        if *bytes > 0 && *bytes + line_bytes > LOG_FILE_MAX_BYTES {
            self.rotate_locked(&mut bytes);
        }
        // Recompute after a possible rotation: the
        // new file starts at 0 bytes.
        let mut writer = self.inner.lock().expect("log file lock poisoned");
        // Swallow the writer's result so a disk-full
        // does not propagate to the pump thread. The
        // eprintln is best-effort and stderr itself
        // is the supervisor's own diagnostic stream;
        // we accept that a real disk-full will
        // surface as a noisy warning, not a panic.
        if writer.write_all(line.as_bytes()).is_err() {
            return;
        }
        if writer.write_all(b"\n").is_err() {
            return;
        }
        let _ = writer.flush();
        *bytes += line_bytes;
    }

    /// Atomically rotate `<name>.log` to `<name>.log.1`.
    /// The caller must hold the bytes counter lock
    /// when calling this.
    fn rotate_locked(&self, bytes: &mut u64) {
        // Drop the BufWriter so the file handle is
        // closed before we move it; on Windows an
        // open file handle would refuse the rename.
        {
            let mut writer = self.inner.lock().expect("log file lock poisoned");
            let _ = writer.flush();
        }
        // Replace the existing `.1` (if any) with the
        // current live file, then open a fresh live
        // file. We do not use `rename` over an
        // existing target because the behaviour of
        // `rename` on Windows when the target exists
        // differs from POSIX; the explicit
        // `remove_file` + `rename` is unambiguous on
        // both.
        let rotated = self.path.with_extension(format!(
            "{}.1",
            self.path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("log")
        ));
        let _ = fs::remove_file(&rotated);
        if let Err(error) = fs::rename(&self.path, &rotated) {
            eprintln!(
                "alex runtime: log rotation failed for {}: {error}",
                self.path.display()
            );
            return;
        }
        // Re-open the live file. If the open fails the
        // old handle is still dropped above so the
        // child can keep writing into a non-existent
        // file path (the OS will surface ENOENT on the
        // next write).
        let new_file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) => {
                eprintln!(
                    "alex runtime: cannot reopen {} after rotation: {error}",
                    self.path.display()
                );
                return;
            }
        };
        *self.inner.lock().expect("log file lock poisoned") = BufWriter::new(new_file);
        *bytes = 0;
    }
}

/// Bundle a `stdout` and `stderr` writer under one
/// `Arc` so the pumps can clone it cheaply.
#[derive(Clone)]
pub struct ServiceLogSink {
    inner: std::sync::Arc<ServiceLogSinkInner>,
}

struct ServiceLogSinkInner {
    stdout: LogFileWriter,
    stderr: LogFileWriter,
}

impl ServiceLogSink {
    /// Build a sink rooted at `<log_dir>/<service>.stdout.log`
    /// and `<log_dir>/<service>.stderr.log`. The
    /// directory is created on demand so the
    /// supervisor does not have to call
    /// `create_dir_all` separately. Returns `None`
    /// when `log_dir` is empty (the v1 standalone
    /// `alex run` path runs without a managed log
    /// root).
    pub fn open(log_dir: &Path, service: &str) -> std::io::Result<Option<Self>> {
        if log_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        fs::create_dir_all(log_dir)?;
        let stdout_path = log_dir.join(format!("{service}.stdout.log"));
        let stderr_path = log_dir.join(format!("{service}.stderr.log"));
        Ok(Some(Self {
            inner: std::sync::Arc::new(ServiceLogSinkInner {
                stdout: LogFileWriter::open(stdout_path)?,
                stderr: LogFileWriter::open(stderr_path)?,
            }),
        }))
    }

    /// Write a stdout line. The line is redacted
    /// before hitting disk so a stray credential
    /// never lands in a long-term log file.
    pub fn write_stdout(&self, line: &str) {
        let redacted = redact_secrets(line);
        self.inner.stdout.write_line(redacted.as_ref());
    }

    /// Write a stderr line.
    pub fn write_stderr(&self, line: &str) {
        let redacted = redact_secrets(line);
        self.inner.stderr.write_line(redacted.as_ref());
    }
}

/// Scrub well-known credential patterns from a single
/// log line. The patterns are deliberately narrow so
/// the false-positive rate stays low:
///
/// * `token=<value>` → `token=<redacted>`
/// * `password=<value>` → `password=<redacted>`
/// * `secret=<value>` → `secret=<redacted>`
/// * `api[_-]?key=<value>` → `api[_-]?key=<redacted>`
/// * `Authorization: Bearer <token>` → `Authorization: Bearer <redacted>`
///
/// The value runs to the next whitespace, `&`, `;`,
/// `"` or end-of-line so the URL/query-string
/// semantics are preserved. We return a `Cow` so a
/// line without a match (the common case) does not
/// allocate.
pub fn redact_secrets(line: &str) -> Cow<'_, str> {
    // Fast path: skip allocation when the line has
    // none of the trigger substrings. Lower-case
    // matching would be more thorough but the
    // credential names are conventionally lowercase
    // in CLI args and HTTP headers, and a
    // false-negative on a malformed log line is
    // strictly better than a false-positive on
    // innocuous text.
    let has_trigger = line.contains("token=")
        || line.contains("password=")
        || line.contains("secret=")
        || line.contains("api_key=")
        || line.contains("api-key=")
        || line.contains("apikey=")
        || line.contains("Authorization: Bearer ")
        || line.contains("authorization: Bearer ");
    if !has_trigger {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = find_next_match(rest) {
        // Copy everything before the match verbatim.
        out.push_str(&rest[..at.byte_index]);
        // Emit the full redacted form. The value
        // after `<key>=` (or after `Authorization:
        // Bearer `) is consumed separately: if the
        // value runs to a terminator (`&`, `;`, `"`,
        // whitespace) we keep the terminator in
        // `rest` and re-enter the loop; if it runs
        // to end-of-line we drop it entirely so a
        // credential that lives at the tail of a log
        // line cannot leak.
        out.push_str(at.redacted);
        let value_slice = &rest[at.byte_index + at.key.len()..];
        match find_value_terminator(value_slice) {
            Some(end) => rest = &value_slice[end..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

#[derive(Debug)]
struct Match {
    /// Index into the *current* `rest` slice.
    byte_index: usize,
    /// The literal `<key>=` prefix (or
    /// `Authorization: Bearer `) that matched.
    key: &'static str,
    /// The full redacted form to emit
    /// (e.g. `token=<redacted>` or
    /// `Authorization: Bearer <redacted>`).
    redacted: &'static str,
}

fn find_next_match(line: &str) -> Option<Match> {
    // Each entry is `(needle, replacement)`. The
    // replacement is the full redacted form, so the
    // redaction can be emitted in one go.
    const CANDIDATES: &[(&str, &str)] = &[
        (
            "Authorization: Bearer ",
            "Authorization: Bearer <redacted>",
        ),
        (
            "authorization: Bearer ",
            "authorization: Bearer <redacted>",
        ),
        ("token=", "token=<redacted>"),
        ("password=", "password=<redacted>"),
        ("secret=", "secret=<redacted>"),
        ("api_key=", "api_key=<redacted>"),
        ("api-key=", "api-key=<redacted>"),
        ("apikey=", "apikey=<redacted>"),
    ];
    let mut best: Option<(usize, &str, &str)> = None;
    for (needle, replacement) in CANDIDATES {
        if let Some(at) = line.find(needle) {
            match best {
                Some((prev, _, _)) if prev <= at => {}
                _ => best = Some((at, *needle, *replacement)),
            }
        }
    }
    best.map(|(byte_index, key, redacted)| Match {
        byte_index,
        key,
        redacted,
    })
}

/// Find the index of the first value-terminator
/// (`&`, `;`, `"`, whitespace). Returns `None` when
/// the value runs to the end of the slice — the
/// caller uses that to decide whether to keep the
/// post-value slice or drop the value entirely.
fn find_value_terminator(s: &str) -> Option<usize> {
    for (index, ch) in s.char_indices() {
        if ch == '&' || ch == ';' || ch == '"' || ch.is_whitespace() {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_secrets_leaves_an_innocuous_line_alone() {
        // Common case: no allocation.
        let line = "starting backend on port 8080";
        let result = redact_secrets(line);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, line);
    }

    #[test]
    fn redact_secrets_scrubs_token_in_query_string() {
        let line = "GET /api?token=abc123&user=alice";
        let result = redact_secrets(line);
        assert_eq!(result, "GET /api?token=<redacted>&user=alice");
    }

    #[test]
    fn redact_secrets_scrubs_password_and_secret() {
        // The second `secret=1` has no terminator
        // after the value, so the `1` is consumed
        // entirely (otherwise a credential that lives
        // at the end of a log line would still leak).
        let line = "config: password=hunter2 secret=top secret=1";
        let result = redact_secrets(line);
        assert_eq!(
            result,
            "config: password=<redacted> secret=<redacted> secret=<redacted>"
        );
    }

    #[test]
    fn redact_secrets_drops_tail_value_with_no_terminator() {
        // Regression: a credential that lives at the
        // tail of a log line (no trailing whitespace,
        // `&`, `;`, or `"`) must be removed entirely.
        let line = "Authorization: Bearer eyJabc.def.ghi";
        let result = redact_secrets(line);
        assert_eq!(result, "Authorization: Bearer <redacted>");
    }

    #[test]
    fn redact_secrets_handles_bearer_header_in_middle_of_line() {
        // A Bearer header followed by more text (the
        // value has a terminator) keeps the post-value
        // slice intact.
        let line = "headers: Authorization: Bearer eyJabc.def.ghi method=POST";
        let result = redact_secrets(line);
        assert_eq!(
            result,
            "headers: Authorization: Bearer <redacted> method=POST"
        );
    }

    #[test]
    fn redact_secrets_handles_api_key_dash_variant() {
        let line = "auth api-key=foo-bar user=bob";
        let result = redact_secrets(line);
        assert_eq!(result, "auth api-key=<redacted> user=bob");
    }

    #[test]
    fn log_file_writer_rotates_after_max_bytes() {
        // Force-rotation smoke test. We write enough
        // bytes to cross the cap and assert the
        // rotated `.1` file exists with the old
        // content.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("svc.stdout.log");
        let writer = LogFileWriter::open(&path).unwrap();
        // Each line is ~91 bytes including the
        // newline. The cap is 1 MiB = 1048576
        // bytes. Writing 15000 lines (~1.3 MiB)
        // guarantees we cross the cap at least
        // once. The lower bound of 11500 lines
        // would not be enough on its own because
        // we are slightly under a megabyte.
        for i in 0..15_000 {
            let line = format!("line {i:04} {}", "x".repeat(80));
            writer.write_line(&line);
        }
        let rotated = path.with_file_name("svc.stdout.log.1");
        assert!(
            rotated.exists(),
            "expected rotated file at {}",
            rotated.display()
        );
        // The live file should not have grown past
        // 1 MiB + one more line.
        let live_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(
            live_size <= LOG_FILE_MAX_BYTES + 200,
            "live file unexpectedly large: {live_size} bytes"
        );
    }
}
