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
        match command.command.as_str() {
            "scale-time" => {
                let scaled = command.timestamp * 4;
                println!(
                    "[execute @{}] {} (scaled timestamp: {})",
                    command.timestamp, command.command, scaled
                );
            }
            _ => println!("[execute @{}] {}", command.timestamp, command.command),
        }
    }
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut pipe_name = "timesimulation-demo".to_string();
    let mut ticking = true;

    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    loop {
        let pipe_path = format!(r"\\.\\pipe\\{pipe_name}");

        println!("waiting for named pipe client on {pipe_path}...");
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .in_buffer_size(16 * 1024)
            .out_buffer_size(16 * 1024)
            .create(&pipe_path)?;

        server.connect().await?;
        println!(
            "client attached; send 'start', 'stop', 'rate:<n>', 'pipe:<name>', or timestamp commands"
        );

        let reader = BufReader::new(server);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let lower = trimmed.to_ascii_lowercase();

            if lower == "start" {
                ticking = true;
                println!("ticking resumed; enqueueing will trigger ticks");
                continue;
            }

            if lower == "stop" {
                ticking = false;
                println!("ticking paused; commands will queue until 'start'");
                continue;
            }

            if let Some(rest) = lower.strip_prefix("rate:") {
                match rest.parse::<u64>() {
                    Ok(rate) if rate > 0 => {
                        dispatcher.set_tick_rate(rate);
                        println!("tick rate updated to {rate}");
                    }
                    _ => eprintln!("invalid rate control command: '{trimmed}'"),
                }
                continue;
            }

            if let Some(rest) = lower.strip_prefix("pipe:") {
                let target = rest.trim();
                if target.is_empty() {
                    eprintln!("pipe control requires a name after 'pipe:'");
                    continue;
                }

                pipe_name = target.to_string();
                println!("switching to new pipe name: {pipe_name}");
                break;
            }

            match dispatcher.enqueue_from_pipe(trimmed) {
                Ok(()) => {
                    if ticking {
                        dispatcher.tick();
                    } else {
                        println!("queued without ticking: {trimmed}");
                    }
                }
                Err(err) => eprintln!("failed to parse pipe line '{trimmed}': {err}"),
            }
        }

        println!("pipe listener reset; awaiting next client");
    }
}

#[cfg(not(windows))]
fn main() {
    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    dispatcher
        .enqueue_from_pipe("3:scale-time")
        .expect("scale-time command should parse");

    dispatcher.run_for_ticks(4);
}
