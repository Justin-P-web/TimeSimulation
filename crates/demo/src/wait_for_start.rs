//! Windows-only demo that buffers commands until a start signal is received.
//! After `start`, each enqueue triggers a tick to drive execution.

use timesimulation::{CommandSink, ScheduledCommand};

#[cfg(windows)]
use timesimulation::Dispatcher;

#[cfg(windows)]
use anyhow::Result;

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Default)]
struct LoggingSink;

impl CommandSink for LoggingSink {
    fn publish_time(&mut self, time: u64) {
        println!("[sim-time] {time}");
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        println!("[execute @{}] {}", command.timestamp, command.command);
    }
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_path = r"\\.\\pipe\\timesimulation-wait-for-start";

    println!("waiting for named pipe client on {pipe_path}...");
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .in_buffer_size(16 * 1024)
        .out_buffer_size(16 * 1024)
        .create(pipe_path)?;

    server.connect().await?;
    println!("client attached; enqueue commands and send 'start' to begin ticking");

    let sink = LoggingSink::default();
    let tick_rate = 1u64;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    let reader = BufReader::new(server);
    let mut lines = reader.lines();
    let mut started = false;

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.eq_ignore_ascii_case("start") {
            if started {
                println!("start signal received again; already ticking after enqueues");
            } else {
                println!("starting dispatcher ticks");
                started = true;
                dispatcher.tick();
            }
            continue;
        }

        match dispatcher.enqueue_from_pipe(trimmed) {
            Ok(()) => {
                if started {
                    dispatcher.tick();
                } else {
                    println!("queued command without ticking: {trimmed}");
                }
            }
            Err(err) => eprintln!("failed to parse pipe line '{trimmed}': {err}"),
        }
    }

    println!("pipe client disconnected; shutting down");

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The wait-for-start demo is only available on Windows hosts.");
}
