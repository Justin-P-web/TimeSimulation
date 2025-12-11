//! Interactive client for driving the dispatcher over an asynchronous REPL.
//!
//! The client supports starting and stopping periodic ticks, adjusting the
//! configured tick rate, enqueueing commands, and querying the current
//! simulated time. On Windows targets an optional named pipe listener can
//! ingest commands using the dispatcher pipe format.

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use timesimulation::{CommandSink, Dispatcher, ScheduledCommand, parse_pipe_line};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::time;

#[derive(Debug, Parser)]
#[command(author, version, about = "Interactive dispatcher client")]
struct Args {
    /// Simulated time advanced per tick
    #[arg(short = 'r', long, default_value_t = 1)]
    tick_rate: u64,

    /// Milliseconds between ticks when ticking is enabled
    #[arg(short = 'i', long, default_value_t = 1_000)]
    interval_ms: u64,

    /// Initial simulated timestamp
    #[arg(long, default_value_t = 0)]
    start_time: u64,

    /// Optional named pipe to listen on (Windows only)
    #[cfg(windows)]
    #[arg(long)]
    pipe_name: Option<String>,
}

#[derive(Debug, Default)]
struct StdoutSink;

impl CommandSink for StdoutSink {
    fn publish_time(&mut self, time: u64) {
        println!("[time] {time}");
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        println!("[exec @{}] {}", command.timestamp, command.command);
    }
}

#[derive(Debug)]
enum ReplCommand {
    Start,
    Stop,
    SetRate(u64),
    Enqueue(ScheduledCommand),
    PipeLine(String),
    Now,
    Help,
    Quit,
}

fn parse_repl_command(line: &str) -> Result<ReplCommand, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err("empty input".to_string());
    }

    let mut parts = trimmed.splitn(2, ' ');
    let command = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");

    match command.to_lowercase().as_str() {
        "start" => Ok(ReplCommand::Start),
        "stop" => Ok(ReplCommand::Stop),
        "now" => Ok(ReplCommand::Now),
        "help" => Ok(ReplCommand::Help),
        "quit" | "exit" => Ok(ReplCommand::Quit),
        "rate" => {
            let rate = rest
                .trim()
                .parse::<u64>()
                .map_err(|_| "usage: rate <non-zero u64>".to_string())?;
            if rate == 0 {
                return Err("tick rate must be greater than zero".to_string());
            }
            Ok(ReplCommand::SetRate(rate))
        }
        "enqueue" => {
            let mut enqueue_parts = rest.splitn(2, ' ');
            let timestamp = enqueue_parts
                .next()
                .ok_or_else(|| "usage: enqueue <timestamp> <command>".to_string())?
                .trim()
                .parse::<u64>()
                .map_err(|_| "timestamp must be a non-negative integer".to_string())?;

            let body = enqueue_parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "usage: enqueue <timestamp> <command>".to_string())?;

            Ok(ReplCommand::Enqueue(ScheduledCommand {
                timestamp,
                command: body.to_string(),
            }))
        }
        "pipe" => {
            if rest.trim().is_empty() {
                return Err("usage: pipe <timestamp:command>".to_string());
            }
            Ok(ReplCommand::PipeLine(rest.trim().to_string()))
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

async fn tick_loop(
    dispatcher: Arc<Mutex<Dispatcher<StdoutSink>>>,
    mut ticker: watch::Receiver<bool>,
    interval: Duration,
) {
    let mut interval_timer = time::interval(interval);
    loop {
        tokio::select! {
            _ = interval_timer.tick() => {
                if *ticker.borrow() {
                    let mut guard = dispatcher.lock().await;
                    guard.tick();
                }
            }
            changed = ticker.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
}

async fn queue_forwarder(
    dispatcher: Arc<Mutex<Dispatcher<StdoutSink>>>,
    mut receiver: mpsc::Receiver<ScheduledCommand>,
) {
    while let Some(command) = receiver.recv().await {
        let mut guard = dispatcher.lock().await;
        guard.enqueue(command.timestamp, command.command);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.tick_rate == 0 {
        anyhow::bail!("tick rate must be greater than zero");
    }

    let sink = StdoutSink::default();
    let dispatcher = Arc::new(Mutex::new(Dispatcher::new_with_tick_rate(
        sink,
        args.start_time,
        args.tick_rate,
    )));

    let (command_sender, command_receiver) = mpsc::channel(64);
    let dispatcher_for_queue = dispatcher.clone();
    tokio::spawn(queue_forwarder(dispatcher_for_queue, command_receiver));

    let (tick_sender, tick_receiver) = watch::channel(false);
    let dispatcher_for_tick = dispatcher.clone();
    let tick_interval = Duration::from_millis(args.interval_ms);
    tokio::spawn(tick_loop(dispatcher_for_tick, tick_receiver, tick_interval));

    #[cfg(windows)]
    if let Some(pipe_name) = args.pipe_name.clone() {
        let pipe_sender = command_sender.clone();
        println!("listening for named pipe commands on {pipe_name}");
        tokio::spawn(async move {
            if let Err(err) =
                timesimulation::windows_pipe::listen_for_pipe_commands(&pipe_name, pipe_sender)
                    .await
            {
                eprintln!("named pipe listener error: {err}");
            }
        });
    }

    println!("Interactive dispatcher client ready. Type 'help' for commands.");
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    while let Some(line) = lines.next_line().await? {
        let parsed = match parse_repl_command(&line) {
            Ok(cmd) => cmd,
            Err(err) => {
                eprintln!("{err}");
                continue;
            }
        };

        match parsed {
            ReplCommand::Start => {
                let _ = tick_sender.send(true);
                println!("ticking started (interval: {:?})", tick_interval);
            }
            ReplCommand::Stop => {
                let _ = tick_sender.send(false);
                println!("ticking stopped");
            }
            ReplCommand::SetRate(rate) => {
                let mut guard = dispatcher.lock().await;
                guard.set_tick_rate(rate);
                println!("tick rate updated to {rate}");
            }
            ReplCommand::Enqueue(command) => {
                if command_sender.send(command).await.is_err() {
                    eprintln!("command queue unavailable");
                }
            }
            ReplCommand::PipeLine(line) => match parse_pipe_line(&line) {
                Ok(parsed) => {
                    let scheduled = ScheduledCommand {
                        timestamp: parsed.timestamp,
                        command: parsed.command,
                    };
                    if command_sender.send(scheduled).await.is_err() {
                        eprintln!("command queue unavailable");
                    }
                }
                Err(err) => eprintln!("failed to parse pipe line: {err}"),
            },
            ReplCommand::Now => {
                let guard = dispatcher.lock().await;
                println!("current simulated time: {}", guard.now());
            }
            ReplCommand::Help => {
                println!(
                    "available commands:\n  start                 - begin ticking on the configured interval\n  stop                  - pause ticking\n  rate <n>              - set tick rate to a non-zero integer\n  enqueue <t> <cmd>     - schedule a command at timestamp t\n  pipe <t:cmd>          - parse and enqueue using pipe syntax\n  now                   - print current simulated time\n  help                  - show this message\n  quit | exit           - terminate the client"
                );
            }
            ReplCommand::Quit => break,
        }
    }

    println!("shutting down client");
    Ok(())
}
