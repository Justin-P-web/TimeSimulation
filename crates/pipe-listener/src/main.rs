//! Tokio runtime entry point for dispatching commands from a Windows named pipe.
//!
//! This binary is intended for Windows hosts only because it uses
//! `tokio::windows::named_pipe` under the hood. Other platforms will compile a
//! stub that reports the limitation at runtime.

use clap::Parser;
#[cfg(windows)]
use timesimulation::Dispatcher;
use timesimulation::{CommandSink, ScheduledCommand};

/// Simple sink that logs dispatched events to stdout.
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

/// Configuration for the named pipe listener.
#[derive(Debug, Parser)]
#[command(
    name = "pipe-listener",
    about = "Dispatch commands from a Windows named pipe"
)]
struct Args {
    /// Named pipe identifier (without the \\.\\pipe\\ prefix).
    #[arg(long, default_value = "timesimulation")]
    pipe_name: String,

    /// Simulated time units advanced per tick.
    #[arg(long, default_value_t = 1)]
    tick_rate: u64,

    /// Real-time milliseconds to wait between ticks.
    #[arg(long, default_value_t = 1000)]
    tick_interval_ms: u64,
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::sync::Arc;
    use timesimulation::windows_pipe::{PipeMessage, PipeTickControl, listen_for_pipe_commands};
    use tokio::sync::{Mutex, mpsc};
    use tokio::time::{self, Duration};

    let args = Args::parse();
    let (tx, mut rx) = mpsc::channel::<PipeMessage>(32);
    let pipe_name = args.pipe_name.clone();
    tokio::spawn(async move {
        if let Err(err) = listen_for_pipe_commands(&pipe_name, tx).await {
            eprintln!("named pipe listener error: {err}");
        }
    });

    let dispatcher = Arc::new(Mutex::new(Dispatcher::new_with_tick_rate(
        LoggingSink::default(),
        0,
        args.tick_rate,
    )));

    let mut interval = time::interval(Duration::from_millis(args.tick_interval_ms));
    let mut ticking = false;
    loop {
        tokio::select! {
            Some(message) = rx.recv() => {
                match message {
                    PipeMessage::Command(cmd) => {
                        let mut dispatcher = dispatcher.lock().await;
                        dispatcher.enqueue(cmd.timestamp, cmd.command);
                    }
                    PipeMessage::Control(PipeTickControl::Start) => {
                        ticking = true;
                    }
                    PipeMessage::Control(PipeTickControl::Stop) => {
                        ticking = false;
                    }
                }
            }
            _ = interval.tick() => {
                if ticking {
                    let mut dispatcher = dispatcher.lock().await;
                    dispatcher.tick();
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The pipe-listener binary is only available on Windows hosts.");
}
