//! Coordination logic for deterministic simulated execution.

use crate::clock::SimClock;
use crate::pipe::{PipeParseError, parse_pipe_line};
use crate::scheduler::{CommandScheduler, ScheduledCommand};

/// A trait representing a consumer of scheduled commands and time updates.
pub trait CommandSink {
    /// Called whenever the dispatcher publishes a new simulated timestamp.
    fn publish_time(&mut self, time: u64);

    /// Called when a command is ready for execution at the current time.
    fn execute(&mut self, command: &ScheduledCommand);
}

/// Dispatcher orchestrates simulated time progression, queueing, and execution.
#[derive(Debug)]
pub struct Dispatcher<Sink: CommandSink> {
    clock: SimClock,
    scheduler: CommandScheduler,
    sink: Sink,
}

impl<Sink: CommandSink> Dispatcher<Sink> {
    /// Creates a dispatcher with the provided sink and optional starting time.
    ///
    /// # Parameters
    /// - `sink`: Consumer notified about time updates and ready commands.
    /// - `start_time`: Initial simulated timestamp; defaults to zero when omitted.
    pub fn new_with_clock(sink: Sink, start_time: u64) -> Self {
        Self {
            clock: SimClock::new(start_time),
            scheduler: CommandScheduler::new(),
            sink,
        }
    }

    /// Convenience constructor that starts the clock at zero.
    pub fn new(sink: Sink) -> Self {
        Self::new_with_clock(sink, 0)
    }

    /// Enqueues a command for execution at the given timestamp.
    pub fn enqueue(&mut self, timestamp: u64, command: String) {
        self.scheduler.schedule(timestamp, command);
    }

    /// Parses a raw instruction line and enqueues it on success.
    pub fn enqueue_from_pipe(&mut self, line: &str) -> Result<(), PipeParseError> {
        let parsed = parse_pipe_line(line)?;
        self.enqueue(parsed.timestamp, parsed.command);
        Ok(())
    }

    /// Advances simulated time by the provided delta and dispatches all ready commands.
    pub fn step(&mut self, delta: u64) {
        self.clock.step(delta);
        self.publish_and_dispatch();
    }

    /// Advances simulated time to a specific timestamp and dispatches ready commands.
    pub fn advance_to(&mut self, target: u64) {
        self.clock.advance_to(target);
        self.publish_and_dispatch();
    }

    /// Returns the current simulated timestamp.
    pub fn now(&self) -> u64 {
        self.clock.now()
    }

    fn publish_and_dispatch(&mut self) {
        let now = self.clock.now();
        self.sink.publish_time(now);
        for cmd in self.scheduler.drain_ready(now) {
            self.sink.execute(&cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingSink {
        times: Vec<u64>,
        executed: Vec<ScheduledCommand>,
    }

    impl CommandSink for RecordingSink {
        fn publish_time(&mut self, time: u64) {
            self.times.push(time);
        }

        fn execute(&mut self, command: &ScheduledCommand) {
            self.executed.push(command.clone());
        }
    }

    #[test]
    fn dispatcher_advances_and_executes_in_order() {
        let sink = RecordingSink::default();
        let mut dispatcher = Dispatcher::new_with_clock(sink, 0);

        dispatcher.enqueue(2, "second".to_string());
        dispatcher.enqueue(1, "first".to_string());
        dispatcher.enqueue(3, "third".to_string());

        dispatcher.step(1);
        dispatcher.step(1);
        dispatcher.step(1);

        assert_eq!(dispatcher.sink.times, vec![1, 2, 3]);
        assert_eq!(
            dispatcher.sink.executed,
            vec![
                ScheduledCommand {
                    timestamp: 1,
                    command: "first".to_string(),
                },
                ScheduledCommand {
                    timestamp: 2,
                    command: "second".to_string(),
                },
                ScheduledCommand {
                    timestamp: 3,
                    command: "third".to_string(),
                },
            ]
        );
    }

    #[test]
    fn dispatcher_can_parse_pipe_lines() {
        let sink = RecordingSink::default();
        let mut dispatcher = Dispatcher::new(sink);

        dispatcher
            .enqueue_from_pipe("5:launch")
            .expect("line should parse");
        dispatcher.advance_to(5);

        assert_eq!(dispatcher.sink.executed.len(), 1);
        assert_eq!(dispatcher.sink.executed[0].command, "launch");
    }

    #[test]
    fn dispatcher_ignores_commands_before_time() {
        let sink = RecordingSink::default();
        let mut dispatcher = Dispatcher::new(sink);

        dispatcher.enqueue_from_pipe("10:late").unwrap();
        dispatcher.step(5);

        assert!(dispatcher.sink.executed.is_empty());
        dispatcher.advance_to(10);

        assert_eq!(dispatcher.sink.executed.len(), 1);
        assert_eq!(dispatcher.sink.executed[0].timestamp, 10);
        assert_eq!(dispatcher.sink.times, vec![5, 10]);
    }

    #[test]
    fn dispatcher_keeps_time_monotonic_when_advancing_backward() {
        let sink = RecordingSink::default();
        let mut dispatcher = Dispatcher::new_with_clock(sink, 5);

        dispatcher.advance_to(3);
        dispatcher.step(0);

        assert_eq!(dispatcher.now(), 5);
        assert_eq!(dispatcher.sink.times, vec![5, 5]);
    }
}
