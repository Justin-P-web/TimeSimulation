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
    /// Execute a single tick immediately.
    Tick,
    /// Advance by an explicit delta without changing the tick rate.
    Step(u64),
    /// Jump directly to a target timestamp.
    Advance(u64),
    /// Run the dispatcher for a fixed number of ticks.
    RunTicks(u64),
    /// Update the tick rate used when progressing simulated time.
    SetRate(u64),
    /// Query the current simulated time.
    Now,
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
                match parse_control_instruction(trimmed) {
                    Ok(Some(event)) => {
                        if sender.send(event).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        eprintln!("failed to parse control instruction '{line}': {err}");
                        continue;
                    }
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
fn parse_control_instruction(line: &str) -> Result<Option<PipeEvent>, String> {
    let (command, rest) = match line.splitn(2, ' ').collect::<Vec<_>>().as_slice() {
        [command, rest] => (command.to_ascii_lowercase(), rest.trim()),
        [command] => match command.split_once(':') {
            Some((cmd, rest)) => (cmd.to_ascii_lowercase(), rest.trim()),
            None => (command.to_ascii_lowercase(), ""),
        },
        _ => (String::new(), ""),
    };

    let parsed = match command.as_str() {
        "start" => Some(PipeEvent::Start),
        "stop" => Some(PipeEvent::Stop),
        "pause" => Some(PipeEvent::Pause),
        "tick" => Some(PipeEvent::Tick),
        "step" => {
            let delta = rest
                .parse::<u64>()
                .map_err(|_| "usage: step <non-negative delta>".to_string())?;
            Some(PipeEvent::Step(delta))
        }
        "advance" => {
            let target = rest
                .parse::<u64>()
                .map_err(|_| "usage: advance <target timestamp>".to_string())?;
            Some(PipeEvent::Advance(target))
        }
        "run" => {
            let ticks = rest
                .parse::<u64>()
                .map_err(|_| "usage: run <number of ticks>".to_string())?;
            Some(PipeEvent::RunTicks(ticks))
        }
        "rate" => {
            let rate = rest
                .parse::<u64>()
                .map_err(|_| "usage: rate <non-zero u64>".to_string())?;
            if rate == 0 {
                return Err("tick rate must be greater than zero".to_string());
            }
            Some(PipeEvent::SetRate(rate))
        }
        "now" => Some(PipeEvent::Now),
        _ => None,
    };

    Ok(parsed)
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
