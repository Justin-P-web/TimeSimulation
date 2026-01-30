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
use timesimulation::{CommandPayload, CommandSink, Dispatcher, ScheduledCommand, parse_pipe_line};
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

#[derive(Debug, PartialEq, Eq)]
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

/// Parses a raw REPL line into a structured command understood by the client.
///
/// Leading and trailing whitespace are ignored, and both long and abbreviated
/// command names are accepted where available. Each subcommand validates its
/// required numeric arguments and returns a descriptive usage error when
/// parsing fails or when the input is empty. Unknown commands are surfaced as
/// errors so the caller can report them to the user.
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
                command: CommandPayload::raw(body),
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

/// Advances the dispatcher on a fixed interval while ticking is enabled.
///
/// The loop wakes at the provided interval and performs a tick when the watch
/// channel indicates ticking is active. When the sender side of the watch
/// channel is dropped the loop exits, allowing graceful shutdown.
async fn tick_loop<Sink: CommandSink + Send + 'static>(
    dispatcher: Arc<Mutex<Dispatcher<Sink>>>,
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

/// Consumes events from the pipe listener and applies them to the dispatcher
/// or ticking control channel.
///
/// This task serializes dispatcher access to ensure commands and clock updates
/// occur in order. Sender errors are ignored after printing because they imply
/// the receiver has gone away. Pipe disconnections disable ticking but do not
/// terminate the forwarder, letting future messages resume work.
async fn queue_forwarder<Sink: CommandSink + Send + 'static>(
    dispatcher: Arc<Mutex<Dispatcher<Sink>>>,
    mut receiver: mpsc::Receiver<PipeEvent>,
    tick_sender: watch::Sender<bool>,
) {
    while let Some(message) = receiver.recv().await {
        match message {
            PipeEvent::Scheduled(command) => {
                let mut guard = dispatcher.lock().await;
                guard.enqueue(command.timestamp, command.command);
            }
            PipeEvent::Tick => {
                let mut guard = dispatcher.lock().await;
                guard.tick();
                println!("performed single tick at rate {} (pipe)", guard.tick_rate());
            }
            PipeEvent::Step(delta) => {
                let mut guard = dispatcher.lock().await;
                guard.step(delta);
                println!("advanced clock by {delta} without changing tick rate (pipe)");
            }
            PipeEvent::Advance(target) => {
                let mut guard = dispatcher.lock().await;
                guard.advance_to(target);
                println!("advanced clock to {target} (pipe)");
            }
            PipeEvent::RunTicks(ticks) => {
                let mut guard = dispatcher.lock().await;
                guard.run_for_ticks(ticks);
                println!(
                    "ran {ticks} tick(s) at rate {}; current time: {} (pipe)",
                    guard.tick_rate(),
                    guard.now()
                );
            }
            PipeEvent::SetRate(rate) => {
                let mut guard = dispatcher.lock().await;
                guard.set_tick_rate(rate);
                println!("tick rate updated to {rate} (pipe)");
            }
            PipeEvent::Now => {
                let guard = dispatcher.lock().await;
                println!("current simulated time: {} (pipe)", guard.now());
            }
            PipeEvent::AddFileNote { file, note } => {
                println!("received file note for {file}: {note} (pipe)");
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
/// Sends a single REPL command line to the remote pipe writer with a newline
/// terminator.
///
/// The writer is flushed after each line so interactive users see responses
/// promptly. Write errors are propagated to allow the caller to reconnect or
/// exit when the pipe is no longer available.
async fn forward_repl_line(
    writer: &mut BufWriter<PipeWriteHalf>,
    line: &str,
) -> anyhow::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(windows)]
/// Converts a local REPL command into the string format expected by the remote
/// pipe server.
///
/// Commands that only affect the local client (help/quit) return `None` so the
/// caller can handle them without sending anything. All other commands are
/// encoded using the dispatcher pipe syntax or control verbs accepted by the
/// server.
fn serialize_repl_command(command: &ReplCommand) -> Option<String> {
    match command {
        ReplCommand::Start => Some("start".to_string()),
        ReplCommand::Stop => Some("stop".to_string()),
        ReplCommand::Tick => Some("tick".to_string()),
        ReplCommand::Step(delta) => Some(format!("step {delta}")),
        ReplCommand::Advance(target) => Some(format!("advance {target}")),
        ReplCommand::RunTicks(ticks) => Some(format!("run {ticks}")),
        ReplCommand::SetRate(rate) => Some(format!("rate {rate}")),
        ReplCommand::Enqueue(command) => Some(format!("{}:{}", command.timestamp, command.command)),
        ReplCommand::PipeLine(line) => Some(line.to_string()),
        ReplCommand::Now => Some("now".to_string()),
        ReplCommand::Help | ReplCommand::Quit => None,
    }
}

/// Prints the interactive command reference for the client, including pipe
/// helpers and Windows-only controls.
fn print_help() {
    println!(
        r#"available commands:
  start                 - begin ticking on the configured interval
  stop                  - pause ticking
  tick                  - advance one tick using the current rate
  step <delta>          - advance by an explicit delta
  advance <timestamp>   - jump directly to a target timestamp
  run <ticks>           - run the dispatcher for a fixed number of ticks
  rate <n>              - set tick rate to a non-zero integer
  now                   - print current simulated time
  help                  - show this message
  quit | exit           - terminate the client

pipe helpers:
  enqueue <t> <cmd>     - schedule a command at timestamp t
  pipe <t:cmd>          - parse and enqueue using pipe syntax

windows named pipe controls (when a listener is active on Windows):
  start | stop | pause  - (pipe) control ticking state for connected listeners
  tick                  - (pipe) execute a single tick immediately
  step <delta>          - (pipe) advance by a delta without changing the tick rate
  advance <timestamp>   - (pipe) jump directly to a target timestamp
  run <ticks>           - (pipe) run the dispatcher for a fixed number of ticks
  rate <n>              - (pipe) set tick rate to a non-zero integer
  now                   - (pipe) print the current simulated time
  <timestamp:command>   - (pipe) schedule a command using dispatcher pipe syntax"#
    );
}

/// Runs the REPL client in local mode, executing commands directly against an
/// in-process dispatcher.
///
/// Validates that the tick rate is non-zero before starting and spawns helper
/// tasks for ticking and pipe event forwarding. All user commands are handled
/// until EOF or a quit command. Errors initializing the dispatcher or reading
/// stdin propagate to the caller.
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
/// Connects to a remote dispatcher over Windows named pipes and forwards REPL
/// commands to it.
///
/// The function establishes a bidirectional pipe connection, optionally
/// attaching a second pipe for server output, and then relays user commands
/// until the connection fails or the user quits. Connection and write errors
/// are surfaced so the caller can report them.
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

    println!("remote mode active. type dispatcher commands to send to the server pipe.");

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

        match serialize_repl_command(&parsed) {
            Some(line) => {
                if let Err(err) = forward_repl_line(&mut writer, &line).await {
                    eprintln!("failed to send command: {err}");
                    break;
                }
            }
            None => match parsed {
                ReplCommand::Help => print_help(),
                ReplCommand::Quit => break,
                _ => {}
            },
        }
    }

    println!("closing pipe client");
    Ok(())
}

/// Entry point that selects the transport mode and launches the interactive
/// client.
///
/// On Windows the mode can be toggled between local and pipe-backed operation;
/// other platforms always use local mode. Argument parsing errors are reported
/// by clap before reaching this function, and any runtime failures from the
/// selected client bubble up to terminate the process with an error.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingSink {
        times: Arc<std::sync::Mutex<Vec<u64>>>,
        executed: Arc<std::sync::Mutex<Vec<ScheduledCommand>>>,
    }

    impl CommandSink for RecordingSink {
        fn publish_time(&mut self, time: u64) {
            self.times.lock().unwrap().push(time);
        }

        fn execute(&mut self, command: &ScheduledCommand) {
            self.executed.lock().unwrap().push(command.clone());
        }
    }

    #[test]
    fn parse_repl_command_handles_valid_and_invalid_inputs() {
        // Arrange
        let valid_cases = vec![
            (" start  ", ReplCommand::Start),
            ("stop", ReplCommand::Stop),
            ("tick", ReplCommand::Tick),
            ("step 5", ReplCommand::Step(5)),
            ("advance 42", ReplCommand::Advance(42)),
            ("run 3", ReplCommand::RunTicks(3)),
            ("rate 7", ReplCommand::SetRate(7)),
            (
                "enqueue 9 launch",
                ReplCommand::Enqueue(ScheduledCommand {
                    timestamp: 9,
                    command: CommandPayload::raw("launch"),
                }),
            ),
            ("pipe 4:cmd", ReplCommand::PipeLine("4:cmd".to_string())),
            ("now", ReplCommand::Now),
            ("help", ReplCommand::Help),
            ("quit", ReplCommand::Quit),
            ("exit", ReplCommand::Quit),
        ];

        // Act
        let parsed_valid: Vec<_> = valid_cases
            .iter()
            .map(|(input, _)| parse_repl_command(input).unwrap())
            .collect();

        // Assert
        for ((_, expected), actual) in valid_cases.into_iter().zip(parsed_valid) {
            assert_eq!(actual, expected);
        }

        // Arrange
        let invalid_cases = vec![
            ("", "empty input"),
            ("unknown", "unknown command 'unknown'"),
            ("step x", "usage: step <non-negative delta>"),
            ("advance", "usage: advance <target timestamp>"),
            ("run -1", "usage: run <number of ticks>"),
            ("rate 0", "tick rate must be greater than zero"),
            ("enqueue 5", "usage: enqueue <timestamp> <command>"),
            ("pipe  ", "usage: pipe <timestamp:command>"),
        ];

        // Act & Assert
        for (input, expected_message) in invalid_cases {
            let error = parse_repl_command(input).expect_err("case should fail");
            assert_eq!(error, expected_message);
        }
    }

    #[tokio::test]
    async fn tick_loop_respects_ticker_state() {
        // Arrange
        let sink = RecordingSink::default();
        let times = sink.times.clone();
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new(sink)));
        let (tick_sender, tick_receiver) = watch::channel(false);
        let interval = Duration::from_millis(5);
        let tick_task = tokio::spawn(tick_loop(dispatcher.clone(), tick_receiver, interval));

        // Act
        tokio::time::sleep(Duration::from_millis(12)).await;
        tick_sender.send_replace(true);
        tokio::time::sleep(Duration::from_millis(20)).await;
        tick_sender.send_replace(false);
        drop(tick_sender);
        tick_task.await.expect("tick loop should exit cleanly");

        // Assert
        let recorded_times = times.lock().unwrap().clone();
        assert!(
            recorded_times.len() >= 3,
            "expected multiple ticks while enabled"
        );
        assert!(recorded_times.windows(2).all(|w| w[0] <= w[1]));
    }

    #[tokio::test]
    async fn queue_forwarder_applies_pipe_events() {
        // Arrange
        let sink = RecordingSink::default();
        let times = sink.times.clone();
        let executed = sink.executed.clone();
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new_with_tick_rate(sink, 0, 2)));
        let (pipe_sender, pipe_receiver) = mpsc::channel(8);
        let (tick_sender, tick_receiver) = watch::channel(false);
        let forwarder = tokio::spawn(queue_forwarder(
            dispatcher.clone(),
            pipe_receiver,
            tick_sender,
        ));

        // Act
        pipe_sender
            .send(PipeEvent::Scheduled(ScheduledCommand {
                timestamp: 2,
                command: CommandPayload::raw("mission"),
            }))
            .await
            .unwrap();
        pipe_sender.send(PipeEvent::Start).await.unwrap();
        pipe_sender.send(PipeEvent::Tick).await.unwrap();
        pipe_sender.send(PipeEvent::Step(3)).await.unwrap();
        pipe_sender.send(PipeEvent::Advance(10)).await.unwrap();
        pipe_sender.send(PipeEvent::RunTicks(1)).await.unwrap();
        pipe_sender.send(PipeEvent::Stop).await.unwrap();
        drop(pipe_sender);
        forwarder.await.expect("forwarder should complete");

        // Assert
        assert!(
            !*tick_receiver.borrow(),
            "ticking should be disabled after stop"
        );
        let recorded_times = times.lock().unwrap().clone();
        let recorded_executed = executed.lock().unwrap().clone();
        assert_eq!(recorded_times, vec![2, 5, 10, 12]);
        assert_eq!(recorded_executed.len(), 1);
        assert_eq!(recorded_executed[0].command.as_str(), "mission");
    }
}
