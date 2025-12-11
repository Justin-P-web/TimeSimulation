//! Simulated clock utilities for deterministic time progression.

/// A deterministic simulated clock that only advances when instructed.
///
/// The clock starts at a user-provided timestamp and advances through
/// explicit ticks or multi-step jumps. No wall-clock time is consulted,
/// which keeps execution deterministic and test-friendly.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SimClock {
    current: u64,
}

impl SimClock {
    /// Creates a new clock starting at the given timestamp.
    ///
    /// # Parameters
    /// - `start`: The initial simulated timestamp.
    pub fn new(start: u64) -> Self {
        Self { current: start }
    }

    /// Returns the current simulated timestamp.
    pub fn now(&self) -> u64 {
        self.current
    }

    /// Advances the clock by exactly one tick.
    ///
    /// This is a convenience wrapper over [`step`] with `delta = 1`.
    pub fn tick(&mut self) {
        self.step(1);
    }

    /// Advances the clock by the provided number of ticks.
    ///
    /// # Parameters
    /// - `delta`: The number of ticks to advance; zero is a no-op.
    ///
    /// # Panics
    /// Panics if the addition would overflow `u64`.
    pub fn step(&mut self, delta: u64) {
        self.current = self
            .current
            .checked_add(delta)
            .expect("simulated time overflowed while stepping");
    }

    /// Moves the clock forward to the target timestamp if it is in the future.
    ///
    /// # Parameters
    /// - `target`: Desired timestamp. If earlier than the current time, the
    ///   clock remains unchanged to preserve monotonicity.
    pub fn advance_to(&mut self, target: u64) {
        if target > self.current {
            let delta = target - self.current;
            self.step(delta);
        }
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_ticks_and_steps() {
        let mut clock = SimClock::default();
        clock.tick();
        assert_eq!(clock.now(), 1);

        clock.step(4);
        assert_eq!(clock.now(), 5);
    }

    #[test]
    fn clock_advances_only_forward() {
        let mut clock = SimClock::new(10);
        clock.advance_to(5);
        assert_eq!(clock.now(), 10);

        clock.advance_to(12);
        assert_eq!(clock.now(), 12);
    }
}
