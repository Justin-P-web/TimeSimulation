//! Library entry point for deterministic time simulation utilities.

pub mod clock;
pub mod client;
pub mod dispatcher;
pub mod pipe;
pub mod scheduler;
pub mod windows_pipe;

pub use clock::SimClock;
pub use client::{ClientError, ClientTransport, SimulationClient};
pub use dispatcher::{CommandSink, Dispatcher};
pub use pipe::{PipeCommand, PipeParseError, parse_pipe_line, read_pipe_commands};
pub use scheduler::{CommandScheduler, ScheduledCommand};
