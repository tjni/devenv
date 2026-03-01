//! Shell and PTY management for devenv.
//!
//! This crate provides shell session management with hot-reload support,
//! including PTY spawning, terminal handling, status line rendering,
//! and task execution within the shell environment.

#[cfg(unix)]
pub mod control_pipe;
pub mod dialect;
mod escape;
mod protocol;
mod pty;
mod session;
mod status_line;
mod task_runner;
mod terminal;
mod utf8_accumulator;

// Control pipe (unix-only: uses FIFO via libc::mkfifo and tokio::net::unix::pipe)
#[cfg(unix)]
pub use control_pipe::{ControlMessage, ControlPipe, ControlPipeReceiver};

// Protocol types
pub use protocol::{PtyTaskRequest, PtyTaskResult, ShellCommand, ShellEvent};

// PTY management
pub use pty::{Pty, PtyError, get_terminal_size};

// Terminal utilities
pub use terminal::{RawModeGuard, is_tty};

// Status line
pub use status_line::{StatusLine, StatusState};

// Shared UI constants (used by devenv-tui)
pub use status_line::{
    CHECKMARK, COLOR_ACTIVE, COLOR_ACTIVE_NESTED, COLOR_COMPLETED, COLOR_FAILED, COLOR_HIERARCHY,
    COLOR_INFO, COLOR_INTERACTIVE, COLOR_SECONDARY, SPINNER_FRAMES, SPINNER_INTERVAL_MS, XMARK,
};

// Task execution
pub use task_runner::{PtyTaskRunner, TaskRunnerError};

// Main session
pub use session::{SessionConfig, SessionError, SessionIo, ShellSession, TuiHandoff};

// Re-export for convenience
pub use portable_pty::{CommandBuilder, PtySize};
