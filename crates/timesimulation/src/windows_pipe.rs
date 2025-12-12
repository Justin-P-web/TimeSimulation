//! Asynchronous helpers for consuming commands from Windows named pipes.
//!
//! The routines in this module are only available on Windows targets because
//! they rely on [`tokio::windows::named_pipe`]. Callers should gate usage with
//! conditional compilation when building cross-platform binaries.

use crate::scheduler::ScheduledCommand;
use std::io;
use tokio::sync::mpsc::Sender;

/// Events emitted by the pipe listener.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PipeEvent {
    /// Resume ticking on the configured interval.
    Start,
    /// Stop ticking immediately.
    Stop,
    /// Pause ticking without terminating the listener.
    Pause,
    /// Scheduled command to enqueue in the dispatcher.
    Scheduled(ScheduledCommand),
    /// Notification that the remote client disconnected.
    Disconnected,
}

impl From<ScheduledCommand> for PipeEvent {
    fn from(command: ScheduledCommand) -> Self {
        PipeEvent::Scheduled(command)
    }
}

#[cfg(windows)]
use crate::pipe::parse_pipe_line;
#[cfg(windows)]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;

/// Spawns a listener that accepts pipe clients and forwards parsed commands to
/// the provided scheduler channel.
///
/// # Errors
/// Returns an [`io::Error`] if the named pipe cannot be bound. Per-client
/// parsing errors are logged and skipped without terminating the listener.
#[cfg(windows)]
pub async fn listen_for_pipe_commands(
    pipe_name: &str,
    sender: Sender<PipeEvent>,
) -> io::Result<()> {
    let pipe_path = format!(r"\\.\\pipe\\{}", pipe_name);

    loop {
        let mut connection = ServerOptions::new().create(&pipe_path)?;
        connection.connect().await?;
        let mut client_sender = sender.clone();

        tokio::spawn(async move {
            if let Err(err) = forward_commands(&mut connection, &mut client_sender).await {
                eprintln!("named pipe client error: {err}");
            }
        });
    }
}

#[cfg(windows)]
async fn forward_commands<R>(reader: &mut R, sender: &mut Sender<PipeEvent>) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    loop {
        match lines.next_line().await? {
            Some(line) => {
                let trimmed = line.trim();
                if let Some(event) = parse_control_instruction(trimmed) {
                    if sender.send(event).await.is_err() {
                        break;
                    }
                    continue;
                }

                match parse_pipe_line(trimmed) {
                    Ok(parsed) => {
                        if sender
                            .send(PipeEvent::Scheduled(ScheduledCommand {
                                timestamp: parsed.timestamp,
                                command: parsed.command,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        eprintln!("failed to parse pipe line '{line}': {err}");
                    }
                }
            }
            None => {
                let _ = sender.send(PipeEvent::Disconnected).await;
                return Ok(());
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
fn parse_control_instruction(line: &str) -> Option<PipeEvent> {
    match line.to_ascii_lowercase().as_str() {
        "start" => Some(PipeEvent::Start),
        "stop" => Some(PipeEvent::Stop),
        "pause" => Some(PipeEvent::Pause),
        _ => None,
    }
}

/// Stub implementation used on non-Windows targets to keep the crate
/// cross-platform. Calling this function outside Windows returns an error at
/// runtime.
#[cfg(not(windows))]
pub async fn listen_for_pipe_commands(
    _pipe_name: &str,
    _sender: Sender<PipeEvent>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "named pipes are only available on Windows targets",
    ))
}
