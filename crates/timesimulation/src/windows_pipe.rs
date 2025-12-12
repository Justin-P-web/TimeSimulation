//! Asynchronous helpers for consuming commands from Windows named pipes.
//!
//! The routines in this module are only available on Windows targets because
//! they rely on [`tokio::windows::named_pipe`]. Callers should gate usage with
//! conditional compilation when building cross-platform binaries.

use crate::scheduler::ScheduledCommand;
use std::io;
use tokio::sync::mpsc::Sender;

/// Messages emitted by the pipe listener.
#[derive(Debug, Clone)]
pub enum PipeMessage {
    /// Control whether ticking should be active.
    Control(PipeTickControl),
    /// Scheduled command to enqueue in the dispatcher.
    Command(ScheduledCommand),
}

impl From<ScheduledCommand> for PipeMessage {
    fn from(command: ScheduledCommand) -> Self {
        PipeMessage::Command(command)
    }
}

/// Control signal for dispatcher ticking.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PipeTickControl {
    /// Resume ticking on the configured interval.
    Start,
    /// Pause ticking until explicitly resumed.
    Stop,
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
    sender: Sender<PipeMessage>,
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
async fn forward_commands<R>(reader: &mut R, sender: &mut Sender<PipeMessage>) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if let Some(control) = parse_control_instruction(trimmed) {
            if sender.send(PipeMessage::Control(control)).await.is_err() {
                break;
            }
            continue;
        }

        match parse_pipe_line(trimmed) {
            Ok(parsed) => {
                if sender
                    .send(PipeMessage::Command(ScheduledCommand {
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

    // Pause ticking when the client disconnects to avoid advancing unexpectedly.
    let _ = sender
        .send(PipeMessage::Control(PipeTickControl::Stop))
        .await;

    Ok(())
}

#[cfg(windows)]
fn parse_control_instruction(line: &str) -> Option<PipeTickControl> {
    match line.to_ascii_lowercase().as_str() {
        "start" | "resume" => Some(PipeTickControl::Start),
        "stop" | "pause" => Some(PipeTickControl::Stop),
        _ => None,
    }
}

/// Stub implementation used on non-Windows targets to keep the crate
/// cross-platform. Calling this function outside Windows returns an error at
/// runtime.
#[cfg(not(windows))]
pub async fn listen_for_pipe_commands(
    _pipe_name: &str,
    _sender: Sender<PipeMessage>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "named pipes are only available on Windows targets",
    ))
}
