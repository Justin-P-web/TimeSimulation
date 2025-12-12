//! Interactive client for driving the dispatcher over an asynchronous REPL.
//!
//! The client supports starting and stopping periodic ticks, adjusting the
//! configured tick rate, enqueueing commands, and querying the current
//! simulated time. On Windows targets an optional named pipe listener can
//! ingest commands using the dispatcher pipe format.

use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use timesimulation::windows_pipe::PipeEvent;
use timesimulation::{CommandSink, Dispatcher, ScheduledCommand, parse_pipe_line};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::time;

#[cfg(windows)]
use tokio::io::{AsyncWriteExt, BufWriter, ReadHalf, WriteHalf, split};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

#[cfg(windows)]
type PipeClient = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(windows)]
type PipeReadHalf = ReadHalf<PipeClient>;
#[cfg(windows)]
type PipeWriteHalf = WriteHalf<PipeClient>;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum TransportMode {
    Local,
    #[cfg(windows)]
    Pipe,
}

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

    /// Preferred transport for REPL commands
    #[arg(long, value_enum, default_value_t = TransportMode::Local)]
    mode: TransportMode,

    /// Named pipe endpoint for remote mode
    #[cfg(windows)]
    #[arg(long, default_value = r"\\.\\pipe\\timesimulation-demo")]
    pipe_endpoint: String,

    /// Optional output pipe endpoint for receiving remote logs
    #[cfg(windows)]
    #[arg(long)]
    pipe_output_endpoint: Option<String>,
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
    Tick,
    Step(u64),
    Advance(u64),
    RunTicks(u64),
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
        "tick" => Ok(ReplCommand::Tick),
        "now" => Ok(ReplCommand::Now),
        "help" => Ok(ReplCommand::Help),
        "quit" | "exit" => Ok(ReplCommand::Quit),
        "step" => {
            let delta = rest
                .trim()
                .parse::<u64>()
                .map_err(|_| "usage: step <non-negative delta>".to_string())?;
            Ok(ReplCommand::Step(delta))
        }
        "advance" => {
            let target = rest
                .trim()
                .parse::<u64>()
                .map_err(|_| "usage: advance <target timestamp>".to_string())?;
            Ok(ReplCommand::Advance(target))
        }
        "run" => {
            let ticks = rest
                .trim()
                .parse::<u64>()
                .map_err(|_| "usage: run <number of ticks>".to_string())?;
            Ok(ReplCommand::RunTicks(ticks))
        }
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
    mut receiver: mpsc::Receiver<PipeEvent>,
    tick_sender: watch::Sender<bool>,
) {
    while let Some(message) = receiver.recv().await {
        match message {
            PipeEvent::Scheduled(command) => {
                let mut guard = dispatcher.lock().await;
                guard.enqueue(command.timestamp, command.command);
            }
            PipeEvent::Start => {
                let _ = tick_sender.send(true);
                println!("ticking started (pipe)");
            }
            PipeEvent::Stop | PipeEvent::Pause | PipeEvent::Disconnected => {
                let _ = tick_sender.send(false);
                println!("ticking stopped (pipe)");
            }
        }
    }
}

#[cfg(windows)]
async fn forward_repl_line(
    writer: &mut BufWriter<PipeWriteHalf>,
    line: &str,
) -> anyhow::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn print_help() {
    println!(
        r#"available commands:
  start                 - begin ticking on the configured interval
  stop                  - pause ticking
  tick                  - advance one tick using the current rate
  step <delta>          - advance by an explicit delta
  advance <timestamp>   - jump directly to a target timestamp
  run <ticks>           - run the dispatcher for a fixed number of ticks
  rate <n>              - set tick rate to a non-zero integer (local mode only)
  now                   - print current simulated time
  help                  - show this message
  quit | exit           - terminate the client

pipe helpers:
  enqueue <t> <cmd>     - schedule a command at timestamp t
  pipe <t:cmd>          - parse and enqueue using pipe syntax

windows named pipe controls (when a listener is active on Windows):
  start                 - (pipe) resume ticking for connected listeners
  stop | pause          - (pipe) stop ticking for connected listeners
  <timestamp:command>   - (pipe) schedule a command using dispatcher pipe syntax
  (pipe listener does not accept rate changes or transport switches)"#
    );
}

async fn run_local_client(args: &Args) -> anyhow::Result<()> {
    if args.tick_rate == 0 {
        anyhow::bail!("tick rate must be greater than zero");
    }

    let sink = StdoutSink::default();
    let dispatcher = Arc::new(Mutex::new(Dispatcher::new_with_tick_rate(
        sink,
        args.start_time,
        args.tick_rate,
    )));

    let (tick_sender, tick_receiver) = watch::channel(false);
    let dispatcher_for_tick = dispatcher.clone();
    let tick_interval = Duration::from_millis(args.interval_ms);
    tokio::spawn(tick_loop(dispatcher_for_tick, tick_receiver, tick_interval));

    let (command_sender, command_receiver) = mpsc::channel(64);
    let dispatcher_for_queue = dispatcher.clone();
    let tick_sender_for_queue = tick_sender.clone();
    tokio::spawn(queue_forwarder(
        dispatcher_for_queue,
        command_receiver,
        tick_sender_for_queue,
    ));

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
            ReplCommand::Tick => {
                let mut guard = dispatcher.lock().await;
                guard.tick();
                println!("performed single tick at rate {}", guard.tick_rate());
            }
            ReplCommand::Step(delta) => {
                let mut guard = dispatcher.lock().await;
                guard.step(delta);
                println!("advanced clock by {delta} without changing tick rate");
            }
            ReplCommand::Advance(target) => {
                let mut guard = dispatcher.lock().await;
                guard.advance_to(target);
                println!("advanced clock to {target}");
            }
            ReplCommand::RunTicks(ticks) => {
                let mut guard = dispatcher.lock().await;
                guard.run_for_ticks(ticks);
                println!(
                    "ran {ticks} tick(s) at rate {}; current time: {}",
                    guard.tick_rate(),
                    guard.now()
                );
            }
            ReplCommand::SetRate(rate) => {
                let mut guard = dispatcher.lock().await;
                guard.set_tick_rate(rate);
                println!("tick rate updated to {rate}");
            }
            ReplCommand::Enqueue(command) => {
                if command_sender
                    .send(PipeEvent::Scheduled(command))
                    .await
                    .is_err()
                {
                    eprintln!("command queue unavailable");
                }
            }
            ReplCommand::PipeLine(line) => match parse_pipe_line(&line) {
                Ok(parsed) => {
                    let scheduled = ScheduledCommand {
                        timestamp: parsed.timestamp,
                        command: parsed.command,
                    };
                    if command_sender
                        .send(PipeEvent::Scheduled(scheduled))
                        .await
                        .is_err()
                    {
                        eprintln!("command queue unavailable");
                    }
                }
                Err(err) => eprintln!("failed to parse pipe line: {err}"),
            },
            ReplCommand::Now => {
                let guard = dispatcher.lock().await;
                println!("current simulated time: {}", guard.now());
            }
            ReplCommand::Help => print_help(),
            ReplCommand::Quit => break,
        }
    }

    println!("shutting down client");
    Ok(())
}

#[cfg(windows)]
async fn run_pipe_client(args: &Args) -> anyhow::Result<()> {
    println!("connecting to dispatcher pipe at {}", args.pipe_endpoint);
    let client = ClientOptions::new().open(&args.pipe_endpoint)?;
    let (read_half, write_half) = split(client);
    let mut writer = BufWriter::new(write_half);

    let mut output_reader: BufReader<PipeReadHalf> =
        if let Some(output_endpoint) = args.pipe_output_endpoint.as_ref() {
            println!("attaching to output pipe at {output_endpoint}");
            let output_client = ClientOptions::new().open(output_endpoint)?;
            let (output_read_half, _) = split(output_client);
            BufReader::new(output_read_half)
        } else {
            BufReader::new(read_half)
        };

    tokio::spawn(async move {
        let mut output_lines = output_reader.lines();
        while let Ok(Some(line)) = output_lines.next_line().await {
            println!("{line}");
        }
    });

    println!("remote mode active. type 'enqueue' or 'pipe' commands to send to the dispatcher.");

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
            ReplCommand::Enqueue(command) => {
                let pipe_line = format!("{}:{}", command.timestamp, command.command);
                if let Err(err) = forward_repl_line(&mut writer, &pipe_line).await {
                    eprintln!("failed to send command: {err}");
                    break;
                }
            }
            ReplCommand::PipeLine(line) => {
                if let Err(err) = forward_repl_line(&mut writer, &line).await {
                    eprintln!("failed to send pipe line: {err}");
                    break;
                }
            }
            ReplCommand::Help => print_help(),
            ReplCommand::Quit => break,
            ReplCommand::Start
            | ReplCommand::Stop
            | ReplCommand::Tick
            | ReplCommand::Step(_)
            | ReplCommand::Advance(_)
            | ReplCommand::RunTicks(_)
            | ReplCommand::SetRate(_)
            | ReplCommand::Now => {
                eprintln!("command is only available in local mode");
            }
        }
    }

    println!("closing pipe client");
    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    #[cfg(windows)]
    {
        match args.mode {
            TransportMode::Local => run_local_client(&args).await?,
            TransportMode::Pipe => run_pipe_client(&args).await?,
        }
    }

    #[cfg(not(windows))]
    run_local_client(&args).await?;

    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ServerOptions;

    #[tokio::test]
    async fn pipe_client_can_split_and_exchange_messages() {
        let pipe_name = format!(
            r"\\\\.\\pipe\\timesimulation-client-test-{}",
            std::process::id()
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .expect("server pipe should create");

        let server_task = tokio::spawn(async move {
            let mut server = server;
            server
                .connect()
                .await
                .expect("server should accept connection");

            let mut received = [0u8; 5];
            server
                .read_exact(&mut received)
                .await
                .expect("server should read client message");
            assert_eq!(&received, b"ping\n");

            server
                .write_all(b"pong\n")
                .await
                .expect("server should respond");
        });

        let client = ClientOptions::new()
            .open(&pipe_name)
            .expect("client should connect to server pipe");
        let (read_half, write_half) = split(client);
        let mut writer = BufWriter::new(write_half);

        forward_repl_line(&mut writer, "ping")
            .await
            .expect("client should send message");

        let mut reader = BufReader::new(read_half);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .await
            .expect("client should read server response");

        assert_eq!(response, "pong\n");
        server_task.await.expect("server task should complete");
    }
}
