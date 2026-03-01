//! FIFO-based task execution in PTY.
//!
//! Uses a named pipe (FIFO) as a control channel for structured messages
//! (shell readiness, task completion with exit code) and a fence marker
//! in PTY output for framing task output.

use crate::control_pipe::{ControlMessage, ControlPipeReceiver};
use crate::protocol::{PtyTaskRequest, PtyTaskResult};
use crate::pty::Pty;
use shell_escape::unix::escape;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;
use strip_ansi_escapes::strip_str;
use thiserror::Error;
use tokio::sync::mpsc;

/// Errors that can occur during task execution.
#[derive(Debug, Error)]
pub enum TaskRunnerError {
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("shell closed before ready")]
    ShellNotReady,
    #[error("spawn_blocking failed: {0}")]
    SpawnBlocking(String),
    #[error("control pipe error: {0}")]
    ControlPipe(String),
}

/// Validate that a string is a valid shell variable name (`[a-zA-Z_][a-zA-Z0-9_]*`).
fn is_valid_shell_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// FIFO-based task execution in PTY.
///
/// Uses a control pipe for structured messages and a fence marker in PTY
/// output for output framing:
/// ```text
/// <command>; __e=$?; echo '{"type":"task_end",...}' >&3; echo __DEVENV_FENCE_<id>__
/// ```
///
/// The fd 3 write happens before the fence echo, so the control message
/// is guaranteed to be in the FIFO kernel buffer by the time we see the
/// fence in PTY output. We then read the control pipe sequentially after
/// fence detection.
pub struct PtyTaskRunner {
    pty: Arc<Pty>,
    control_rx: ControlPipeReceiver,
}

impl PtyTaskRunner {
    /// Create a new task runner for the given PTY and control pipe receiver.
    pub fn new(pty: Arc<Pty>, control_rx: ControlPipeReceiver) -> Self {
        Self { pty, control_rx }
    }

    /// Wait for shell to signal readiness via control pipe.
    ///
    /// Concurrently drains PTY output to prevent kernel buffer fill.
    /// When `vt` is `Some`, feeds PTY output into the virtual terminal;
    /// when `None`, discards it.
    pub async fn wait_for_shell_ready(
        &mut self,
        mut vt: Option<&mut avt::Vt>,
    ) -> Result<(), TaskRunnerError> {
        tracing::trace!("wait_for_shell_ready: waiting for shell to be ready");

        loop {
            let pty_clone = Arc::clone(&self.pty);
            let pty_read = tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 4096];
                pty_clone.read(&mut buf).map(|n| (buf, n))
            });
            tokio::pin!(pty_read);

            tokio::select! {
                msg = self.control_rx.recv() => {
                    // Await the in-flight PTY read so no orphaned thread
                    // holds the reader mutex after we return.
                    let _ = pty_read.await;

                    match msg {
                        Ok(Some(ControlMessage::Ready)) => {
                            tracing::trace!("wait_for_shell_ready: shell is ready");
                            return Ok(());
                        }
                        Ok(Some(other)) => {
                            tracing::warn!("wait_for_shell_ready: unexpected message: {:?}", other);
                        }
                        Ok(None) => {
                            tracing::error!("wait_for_shell_ready: control pipe closed");
                            return Err(TaskRunnerError::ShellNotReady);
                        }
                        Err(e) => {
                            tracing::error!("wait_for_shell_ready: control pipe error: {}", e);
                            return Err(TaskRunnerError::ControlPipe(e.to_string()));
                        }
                    }
                }
                result = &mut pty_read => {
                    match result {
                        Ok(Ok((buf, n))) if n > 0 => {
                            if let Some(ref mut vt) = vt {
                                let chunk = String::from_utf8_lossy(&buf[..n]);
                                vt.feed_str(&chunk);
                            }
                            tracing::trace!("wait_for_shell_ready: read {} bytes", n);
                        }
                        Ok(Ok(_)) => {
                            tracing::error!("wait_for_shell_ready: PTY closed before ready");
                            return Err(TaskRunnerError::ShellNotReady);
                        }
                        Ok(Err(e)) => {
                            tracing::error!("wait_for_shell_ready: PTY read error: {}", e);
                            return Err(TaskRunnerError::Io(e));
                        }
                        Err(e) => {
                            tracing::error!("wait_for_shell_ready: spawn_blocking failed: {}", e);
                            return Err(TaskRunnerError::SpawnBlocking(e.to_string()));
                        }
                    }
                }
            }
        }
    }

    /// Execute a single task request and send result via response channel.
    ///
    /// When `vt` is `Some`, feeds PTY output into the virtual terminal;
    /// when `None`, output is only captured for the task result.
    pub async fn execute(&mut self, request: PtyTaskRequest, mut vt: Option<&mut avt::Vt>) {
        let id = request.id;
        let fence = format!("__DEVENV_FENCE_{id}__");

        tracing::trace!("execute: task id={}, cmd={}", id, request.command);

        // Build command with control pipe signaling and fence
        let mut cmd_parts = Vec::new();

        // Set environment variables (validate keys to prevent shell injection)
        for (key, value) in &request.env {
            if !is_valid_shell_var_name(key) {
                tracing::warn!("execute: skipping invalid env key: {:?}", key);
                continue;
            }
            let escaped = escape(Cow::Borrowed(value));
            cmd_parts.push(format!("export {key}={escaped}"));
        }

        // Change directory if specified
        if let Some(ref cwd) = request.cwd {
            let escaped = escape(Cow::Borrowed(cwd.as_str()));
            cmd_parts.push(format!("cd {escaped}"));
        }

        // Execute the command
        cmd_parts.push(request.command.clone());

        // Signal exit code via control pipe, then echo fence to PTY.
        // The fd 3 write is sequential before the fence echo, so the
        // control message is guaranteed to be in the FIFO kernel buffer
        // by the time we see the fence in PTY output.
        cmd_parts.push(format!(
            "__e=$?; echo '{{\"type\":\"task_end\",\"id\":{id},\"exit_code\":'$__e'}}' >&3; echo '{fence}'"
        ));

        // Join with semicolons and add newline.
        // History is disabled during task execution (set +o history in rcfile),
        // so these commands won't appear in user's shell history.
        let full_cmd = if vt.is_some() {
            // Prefix with space to prevent command from being saved to shell history
            format!(" {}\n", cmd_parts.join("; "))
        } else {
            format!("{}\n", cmd_parts.join("; "))
        };

        // Write command to PTY in a background task so reading can start
        // immediately. The PTY kernel buffer is only ~4-16KB on macOS; if the
        // command string (with inline exports) exceeds that, a synchronous
        // write_all would deadlock because bash echoes input back and nobody
        // is draining the read side.
        tracing::trace!("execute: writing command to PTY:\n{}", full_cmd);
        let pty_write = Arc::clone(&self.pty);
        let cmd_bytes = full_cmd.into_bytes();
        let write_handle = tokio::task::spawn_blocking(move || {
            pty_write.write_all(&cmd_bytes)?;
            pty_write.flush()
        });

        // Read PTY output until we see the fence
        let mut output_buffer = String::new();
        let mut stdout_lines = Vec::new();
        let mut error_msg: Option<String> = None;

        'read_loop: loop {
            let pty_clone = Arc::clone(&self.pty);
            let read_result = tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 4096];
                match pty_clone.read(&mut buf) {
                    Ok(n) => Ok((buf, n)),
                    Err(e) => Err(e),
                }
            })
            .await;

            let (buf, n) = match read_result {
                Ok(Ok((buf, n))) => (buf, n),
                Ok(Err(e)) => {
                    error_msg = Some(format!("PTY read error: {e}"));
                    break 'read_loop;
                }
                Err(e) => {
                    error_msg = Some(format!("spawn_blocking failed: {e}"));
                    break 'read_loop;
                }
            };

            if n == 0 {
                tracing::trace!("execute: PTY returned 0 bytes (closed)");
                error_msg = Some("PTY closed unexpectedly".to_string());
                break 'read_loop;
            }

            let chunk = String::from_utf8_lossy(&buf[..n]);
            if let Some(ref mut vt) = vt {
                vt.feed_str(&chunk);
            }
            tracing::trace!("execute: read {} bytes: {:?}", n, chunk);
            output_buffer.push_str(&chunk);

            // Process complete lines
            while let Some(newline_pos) = output_buffer.find('\n') {
                let line = output_buffer[..newline_pos].to_string();
                output_buffer = output_buffer[newline_pos + 1..].to_string();

                // Strip ANSI codes and trim whitespace for fence detection
                let clean = strip_str(&line);
                let trimmed = clean.trim();

                tracing::trace!("execute: line: {:?}", trimmed);

                // Check for fence
                if trimmed == fence {
                    tracing::trace!("execute: found fence");
                    break 'read_loop;
                }

                stdout_lines.push((Instant::now(), line));
            }
        }

        // Get exit code from control pipe (data is in kernel buffer after fence)
        let exit_code = if error_msg.is_none() {
            match self.control_rx.recv().await {
                Ok(Some(ControlMessage::TaskEnd {
                    id: msg_id,
                    exit_code,
                })) if msg_id == id => {
                    tracing::trace!("execute: got exit_code={} from control pipe", exit_code);
                    Some(exit_code)
                }
                Ok(Some(other)) => {
                    error_msg = Some(format!("unexpected control message: {:?}", other));
                    None
                }
                Ok(None) => {
                    error_msg = Some("control pipe closed".to_string());
                    None
                }
                Err(e) => {
                    error_msg = Some(format!("control pipe error: {e}"));
                    None
                }
            }
        } else {
            None
        };

        // Check if the background write succeeded
        if error_msg.is_none() {
            match write_handle.await {
                Ok(Err(e)) => error_msg = Some(format!("Failed to write to PTY: {e}")),
                Err(e) => error_msg = Some(format!("PTY write task panicked: {e}")),
                Ok(Ok(())) => {}
            }
        }

        // Build result
        let (success, error) = if let Some(err) = error_msg {
            (false, Some(err))
        } else {
            let code = exit_code.unwrap_or(1);
            (
                code == 0,
                if code == 0 {
                    None
                } else {
                    Some(format!("Task exited with code {code}"))
                },
            )
        };

        tracing::trace!("execute: result success={}, error={:?}", success, error);

        let _ = request.response_tx.send(PtyTaskResult {
            success,
            stdout_lines,
            stderr_lines: Vec::new(),
            error,
        });
    }

    /// Disable PROMPT_COMMAND to avoid ~100ms+ overhead per task from prompt hooks.
    /// History is already disabled from shell init, so we just save/clear PROMPT_COMMAND.
    fn disable_prompt_command(&self) -> Result<(), TaskRunnerError> {
        self.pty
            .write_all(b"__devenv_saved_pc=\"$PROMPT_COMMAND\"; PROMPT_COMMAND=\n")
            .map_err(|e| {
                TaskRunnerError::Pty(format!("Failed to disable PROMPT_COMMAND: {}", e))
            })?;
        self.pty
            .flush()
            .map_err(|e| TaskRunnerError::Pty(format!("Failed to flush PTY: {}", e)))
    }

    /// Restore PROMPT_COMMAND when handing control to user.
    /// History is re-enabled separately inside drain_pty_to_vt() so the
    /// fence echo commands are never recorded.
    fn restore_prompt_command(&self) -> Result<(), TaskRunnerError> {
        self.pty
            .write_all(b"PROMPT_COMMAND=\"$__devenv_saved_pc\"\n")
            .map_err(|e| {
                TaskRunnerError::Pty(format!("Failed to restore PROMPT_COMMAND: {}", e))
            })?;
        self.pty
            .flush()
            .map_err(|e| TaskRunnerError::Pty(format!("Failed to flush PTY: {}", e)))
    }

    /// Run task loop, processing requests until channel closes.
    pub async fn run_loop(
        &mut self,
        task_rx: &mut mpsc::Receiver<PtyTaskRequest>,
    ) -> Result<(), TaskRunnerError> {
        tracing::trace!("run_loop: waiting for task requests");
        self.disable_prompt_command()?;

        while let Some(request) = task_rx.recv().await {
            self.execute(request, None).await;
        }

        self.restore_prompt_command()?;
        // Re-enable history (no drain in this path, so send directly)
        self.pty
            .write_all(b" set -o history\n")
            .map_err(|e| TaskRunnerError::Pty(format!("Failed to re-enable history: {}", e)))?;
        self.pty
            .flush()
            .map_err(|e| TaskRunnerError::Pty(format!("Failed to flush PTY: {}", e)))?;
        tracing::trace!("run_loop: task channel closed, exiting");
        Ok(())
    }

    /// Run task loop with VT state tracking.
    ///
    /// This is the main entry point for running tasks in the PTY before
    /// terminal handoff. It waits for shell readiness, then processes
    /// task requests until the channel closes.
    pub async fn run_with_vt(
        &mut self,
        task_rx: &mut mpsc::Receiver<PtyTaskRequest>,
        vt: &mut avt::Vt,
        pty_ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), TaskRunnerError> {
        // Wait for shell to be ready
        self.wait_for_shell_ready(Some(&mut *vt)).await?;

        if let Some(tx) = pty_ready_tx {
            let _ = tx.send(());
        }

        tracing::trace!("run_with_vt: waiting for task requests");
        self.disable_prompt_command()?;

        while let Some(request) = task_rx.recv().await {
            self.execute(request, Some(&mut *vt)).await;
        }

        self.restore_prompt_command()?;

        // Drain any pending PTY output into VT so it doesn't leak to stdout later.
        // Uses a single fence to deterministically consume all output without leaving
        // zombie threads that could steal future PTY reads.
        // History is still disabled at this point; the drain command itself
        // re-enables history after the fence so it is never recorded.
        self.drain_pty_to_vt(vt).await;

        tracing::trace!("run_with_vt: task channel closed, exiting");
        Ok(())
    }

    /// Drain pending PTY output into VT using a fence.
    ///
    /// Sends the fence echo twice as separate command lines, then reads
    /// until the fence appears twice. Reading until the second match
    /// ensures the PROMPT_COMMAND output between them is consumed, leaving
    /// exactly one prompt in the buffer. History is re-enabled after the
    /// second fence so the drain commands are never recorded.
    ///
    /// Each read is individually awaited (no infinite loop inside
    /// spawn_blocking), so no zombie threads are left behind that could
    /// steal future PTY reads from the session's reader thread.
    async fn drain_pty_to_vt(&self, vt: &mut avt::Vt) {
        let fence = "__DEVENV_DRAIN__";
        // Two fence echos: reading until the second match consumes
        // the PROMPT_COMMAND output between them. History is re-enabled
        // on the same line as the second echo so its echoback is also
        // consumed before the match.
        let cmd = format!(" echo '{fence}'\n echo '{fence}'; set -o history\n");
        if self.pty.write_all(cmd.as_bytes()).is_err() || self.pty.flush().is_err() {
            tracing::warn!("drain_pty_to_vt: failed to send drain command");
            return;
        }

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let mut buffer = String::new();
        let mut total_bytes = 0usize;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!("drain_pty_to_vt: timed out waiting for fence");
                break;
            }

            let pty_clone = Arc::clone(&self.pty);
            let handle = tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 4096];
                pty_clone.read(&mut buf).map(|n| buf[..n].to_vec())
            });

            match tokio::time::timeout(remaining, handle).await {
                Ok(Ok(Ok(data))) if !data.is_empty() => {
                    let chunk = String::from_utf8_lossy(&data);
                    vt.feed_str(&chunk);
                    total_bytes += data.len();
                    buffer.push_str(&chunk);
                    // Count fence matches (after stripping ANSI codes).
                    // We need TWO matches (one per echo) to consume the
                    // PROMPT_COMMAND output between them.
                    let match_count = buffer
                        .lines()
                        .filter(|line| strip_str(line).trim() == fence)
                        .count();
                    if match_count >= 2 {
                        break;
                    }
                }
                _ => {
                    tracing::warn!("drain_pty_to_vt: read failed while waiting for fence");
                    break;
                }
            }
        }

        if total_bytes > 0 {
            tracing::trace!("drain_pty_to_vt: drained {} bytes", total_bytes);
        }
    }
}
