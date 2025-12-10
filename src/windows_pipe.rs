//! Asynchronous helpers for consuming commands from Windows named pipes.
//!
//! The routines in this module are only available on Windows targets because
//! they rely on [`tokio::windows::named_pipe`]. Callers should gate usage with
//! conditional compilation when building cross-platform binaries.

use crate::scheduler::ScheduledCommand;
use std::io;
use tokio::sync::mpsc::Sender;

#[cfg(windows)]
use crate::pipe::parse_pipe_line;
#[cfg(windows)]
use tokio::io::{AsyncBufReadExt, BufReader};

/// Spawns a listener that accepts pipe clients and forwards parsed commands to
/// the provided scheduler channel.
///
/// # Errors
/// Returns an [`io::Error`] if the named pipe cannot be bound. Per-client
/// parsing errors are logged and skipped without terminating the listener.
#[cfg(windows)]
pub async fn listen_for_pipe_commands(
    pipe_name: &str,
    sender: Sender<ScheduledCommand>,
) -> io::Result<()> {
    use tokio::net::windows::named_pipe::PipeListener;

    let pipe_path = format!(r"\\.\\pipe\\{}", pipe_name);
    let mut listener = PipeListener::bind(&pipe_path)?;

    loop {
        let mut connection = listener.accept().await?;
        let mut client_sender = sender.clone();

        tokio::spawn(async move {
            if let Err(err) = forward_commands(&mut connection, &mut client_sender).await {
                eprintln!("named pipe client error: {err}");
            }
        });
    }
}

#[cfg(windows)]
async fn forward_commands<R>(
    reader: &mut R,
    sender: &mut Sender<ScheduledCommand>,
) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        match parse_pipe_line(&line) {
            Ok(parsed) => {
                if sender
                    .send(ScheduledCommand {
                        timestamp: parsed.timestamp,
                        command: parsed.command,
                    })
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

    Ok(())
}

/// Stub implementation used on non-Windows targets to keep the crate
/// cross-platform. Calling this function outside Windows returns an error at
/// runtime.
#[cfg(not(windows))]
pub async fn listen_for_pipe_commands(
    _pipe_name: &str,
    _sender: Sender<ScheduledCommand>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "named pipes are only available on Windows targets",
    ))
}
