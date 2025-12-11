//! Example binary entry point.
//!
//! # Usage
//!
//! ```bash
//! cargo run -- --tick-rate 5
//! cargo run -- start --tick-rate 5
//! ```
//!
//! The `start` subcommand constructs a dispatcher with the provided tick rate
//! (default: `2`, and used automatically when no subcommand is given), enqueues
//! a sample command, and advances time indefinitely. The `--tick-rate` flag can
//! be passed with or without the `start` subcommand.
//! In real deployments, external processes would feed pipe lines to the
//! dispatcher; here we simply enqueue a sample command for illustration.

use clap::{Parser, Subcommand};

use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};

const DEFAULT_TICK_RATE: u64 = 2;

#[derive(Debug, Parser)]
#[command(name = "timesimulation", about = "Deterministic time simulation demo")]
struct Cli {
    /// Simulated time units advanced per tick.
    #[arg(long, default_value_t = DEFAULT_TICK_RATE, global = true)]
    tick_rate: u64,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the simulation loop.
    Start,
}

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

fn main() {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or(Commands::Start);

    match command {
        Commands::Start => run_simulation(cli.tick_rate),
    }
}

fn run_simulation(tick_rate: u64) {
    let sink = LoggingSink::default();
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    run_forever(&mut dispatcher);
}

fn run_forever(dispatcher: &mut Dispatcher<impl CommandSink>) {
    loop {
        dispatcher.tick();
    }
}
