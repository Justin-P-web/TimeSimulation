//! Scheduling utilities for deterministic simulated command execution.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::command::CommandPayload;

/// A command scheduled for execution at a specific simulated timestamp.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScheduledCommand {
    /// Target timestamp for when the command becomes eligible to run.
    pub timestamp: u64,
    /// Arbitrary command payload supplied by external processes.
    pub command: CommandPayload,
}

impl Ord for ScheduledCommand {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering to make the smallest timestamp pop first from a max-heap.
        other
            .timestamp
            .cmp(&self.timestamp)
            .then_with(|| self.command.cmp(&other.command))
    }
}

impl PartialOrd for ScheduledCommand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A priority queue that stores commands keyed by their scheduled timestamp.
#[derive(Debug, Default)]
pub struct CommandScheduler {
    queue: BinaryHeap<ScheduledCommand>,
}

impl CommandScheduler {
    /// Creates a new, empty scheduler.
    pub fn new() -> Self {
        Self {
            queue: BinaryHeap::new(),
        }
    }

    /// Inserts a command into the queue to run at the provided timestamp.
    ///
    /// # Parameters
    /// - `timestamp`: Simulated timestamp when the command should execute.
    /// - `command`: Arbitrary textual command content.
    pub fn schedule(&mut self, timestamp: u64, command: CommandPayload) {
        self.queue.push(ScheduledCommand { timestamp, command });
    }

    /// Returns `true` when at least one command is ready at `now`.
    pub fn has_ready(&self, now: u64) -> bool {
        self.queue
            .peek()
            .map(|cmd| cmd.timestamp <= now)
            .unwrap_or(false)
    }

    /// Retrieves and removes all commands whose timestamps are less than or equal to `now`.
    pub fn drain_ready(&mut self, now: u64) -> Vec<ScheduledCommand> {
        let mut ready = Vec::new();
        while self
            .queue
            .peek()
            .map(|cmd| cmd.timestamp <= now)
            .unwrap_or(false)
        {
            if let Some(cmd) = self.queue.pop() {
                ready.push(cmd);
            }
        }
        ready
    }

    /// Returns the timestamp of the next scheduled command, if any.
    pub fn next_timestamp(&self) -> Option<u64> {
        self.queue.peek().map(|cmd| cmd.timestamp)
    }

    /// Returns `true` when there are no queued commands.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_orders_by_timestamp() {
        let mut scheduler = CommandScheduler::new();
        scheduler.schedule(5, CommandPayload::raw("later"));
        scheduler.schedule(1, CommandPayload::raw("first"));
        scheduler.schedule(3, CommandPayload::raw("middle"));

        assert!(scheduler.has_ready(1));
        let ready = scheduler.drain_ready(10);
        let timestamps: Vec<u64> = ready.into_iter().map(|c| c.timestamp).collect();
        assert_eq!(timestamps, vec![1, 3, 5]);
    }

    #[test]
    fn scheduler_reports_next_timestamp() {
        let mut scheduler = CommandScheduler::new();
        assert!(scheduler.next_timestamp().is_none());

        scheduler.schedule(7, CommandPayload::raw("cmd"));
        assert_eq!(scheduler.next_timestamp(), Some(7));
    }
}
