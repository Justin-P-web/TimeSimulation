//! Example binary entry point.
//!
//! This binary demonstrates constructing a dispatcher and stepping the
//! simulated clock. In real deployments, external processes feed pipe lines to
//! the dispatcher. On Windows targets, this example connects to a named pipe
//! client before driving the dispatcher from pipe input; other targets fall
//! back to a static demo command.

use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};

#[cfg(windows)]
use anyhow::Result;

#[derive(Debug, Default)]
struct LoggingSink;

impl CommandSink for LoggingSink {
    fn publish_time(&mut self, time: u64) {
        println!("[sim-time] {}", time);
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        println!("[execute @{}] {}", command.timestamp, command.command);
    }
}

#[cfg(windows)]
fn main() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;
    use tokio::runtime::Runtime;

    let pipe_path = r"\\.\\pipe\\timesimulation-demo";
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .in_buffer_size(16 * 1024)
        .out_buffer_size(16 * 1024)
        .create(pipe_path)?;

    let runtime = Runtime::new()?;

    println!("waiting for named pipe client on {pipe_path}...");
    let server = runtime.block_on(async {
        let mut server = server;
        server.connect().await?;
        println!("client attached; starting dispatcher");
        Result::<_, anyhow::Error>::Ok(server)
    })?;

    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    runtime.block_on(async move {
        let mut reader = BufReader::new(server);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            match dispatcher.enqueue_from_pipe(&line) {
                Ok(()) => dispatcher.tick(),
                Err(err) => eprintln!("failed to parse pipe line '{line}': {err}"),
            }
        }

        println!("pipe client disconnected; shutting down");
        Result::<_, anyhow::Error>::Ok(())
    })?;

    Ok(())
}

#[cfg(not(windows))]
fn main() {
    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    dispatcher.run_for_ticks(2);
}
