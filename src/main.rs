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
    let mut dispatcher = Dispatcher::new(sink);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    dispatcher.step(1);
    dispatcher.step(1);
}
