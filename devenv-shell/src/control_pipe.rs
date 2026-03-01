//! Out-of-band control pipe for PTY task runner.
//!
//! Uses a named pipe (FIFO) as a side channel so the shell can send
//! structured JSON messages without mixing them into PTY output.
//! The FIFO path is passed to the shell via `DEVENV_CONTROL_FIFO`
//! env var; the rcfile opens it on fd 3.

use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::unix::pipe;

/// Messages sent by the shell through the control pipe.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Shell initialization is complete.
    Ready,
    /// A task finished executing.
    TaskEnd {
        /// Task ID matching the `PtyTaskRequest.id`.
        id: u64,
        /// Process exit code.
        exit_code: i32,
    },
}

/// Owns the FIFO file on disk. Removes it on drop.
pub struct ControlPipe {
    path: PathBuf,
}

impl ControlPipe {
    /// Create a new FIFO at `path`.
    ///
    /// Returns an error if `mkfifo` fails (e.g. path already exists).
    pub fn create(path: PathBuf) -> io::Result<Self> {
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // SAFETY: c_path is a valid null-terminated C string and 0o600 is a
        // valid mode. mkfifo either creates the FIFO or returns an error code.
        let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { path })
    }

    /// Path to the FIFO (for `DEVENV_CONTROL_FIFO` env var).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the read end and return a receiver.
    ///
    /// The FIFO is opened read-only with `O_NONBLOCK` (tokio's default),
    /// so the open returns immediately even if no writer is connected yet.
    /// Async reads will wait for data via kqueue/epoll.
    pub fn into_receiver(self) -> io::Result<ControlPipeReceiver> {
        let reader = pipe::OpenOptions::new().open_receiver(self.path())?;
        let buf_reader = BufReader::new(reader);

        Ok(ControlPipeReceiver {
            reader: buf_reader,
            _pipe: self,
        })
    }
}

impl Drop for ControlPipe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Reads structured messages from the control FIFO.
pub struct ControlPipeReceiver {
    reader: BufReader<pipe::Receiver>,
    /// Keeps `ControlPipe` alive (and cleans up FIFO on drop).
    _pipe: ControlPipe,
}

impl ControlPipeReceiver {
    /// Read the next control message.
    ///
    /// Returns `None` on EOF (shell closed fd 3).
    /// This is truly async (epoll-based) and works in `tokio::select!`.
    pub async fn recv(&mut self) -> io::Result<Option<ControlMessage>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let msg: ControlMessage = serde_json::from_str(line.trim())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(msg))
    }
}
