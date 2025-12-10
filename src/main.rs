//! Example binary entry point.
//!
//! This binary demonstrates constructing a dispatcher and stepping the
//! simulated clock. In real deployments, external processes would feed pipe
//! lines to the dispatcher; here we simply enqueue a sample command for
//! illustration.

use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};

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
    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    dispatcher.run_for_ticks(2);
}
