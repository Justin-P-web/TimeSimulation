//! Transport-agnostic client wrapper for driving simulated time dispatchers.

use std::io::{self, BufRead};

use crate::dispatcher::{CommandSink, Dispatcher};
use crate::pipe::PipeParseError;

/// Errors that can occur while interacting with a [`SimulationClient`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Parsing failure while consuming pipe input.
    #[error(transparent)]
    Pipe(#[from] PipeParseError),
    /// I/O failure while reading from a transport source.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Interface implemented by transport backends that can drive a dispatcher.
pub trait ClientTransport: Send {
    /// Establishes any underlying connections for the transport.
    fn connect(&mut self) -> Result<(), ClientError>;

    /// Updates the tick rate used when progressing simulated time.
    fn set_tick_rate(&mut self, tick_rate: u64) -> Result<(), ClientError>;

    /// Schedules a command for future execution.
    fn enqueue_command(&mut self, timestamp: u64, command: String) -> Result<(), ClientError>;

    /// Starts processing using the configured transport.
    fn start(&mut self) -> Result<(), ClientError>;

    /// Stops processing for the transport if it is currently active.
    fn stop(&mut self) -> Result<(), ClientError>;
}

/// Client wrapper that delegates to a configurable transport backend.
pub struct SimulationClient {
    transport: Box<dyn ClientTransport>,
}

impl SimulationClient {
    /// Creates a client that owns an in-memory dispatcher for direct testing.
    pub fn from_dispatcher<Sink: CommandSink + Send + 'static>(
        dispatcher: Dispatcher<Sink>,
    ) -> Self {
        Self {
            transport: Box::new(InMemoryTransport::new(dispatcher)),
        }
    }

    /// Creates a client wired to a buffered pipe reader.
    pub fn from_pipe_reader<Sink, R>(dispatcher: Dispatcher<Sink>, reader: R) -> Self
    where
        Sink: CommandSink + Send + 'static,
        R: BufRead + Send + 'static,
    {
        Self {
            transport: Box::new(PipeTransport::new(dispatcher, Box::new(reader))),
        }
    }

    /// Connects the underlying transport, preparing it for use.
    pub fn connect(&mut self) -> Result<(), ClientError> {
        self.transport.connect()
    }

    /// Updates the tick rate used by the transport when advancing simulated time.
    pub fn set_tick_rate(&mut self, tick_rate: u64) -> Result<(), ClientError> {
        self.transport.set_tick_rate(tick_rate)
    }

    /// Enqueues a command for future execution via the configured transport.
    pub fn enqueue_command(&mut self, timestamp: u64, command: String) -> Result<(), ClientError> {
        self.transport.enqueue_command(timestamp, command)
    }

    /// Starts the underlying transport processing.
    pub fn start(&mut self) -> Result<(), ClientError> {
        self.transport.start()
    }

    /// Stops the transport, preventing further processing.
    pub fn stop(&mut self) -> Result<(), ClientError> {
        self.transport.stop()
    }
}

/// In-memory transport that drives a dispatcher directly.
struct InMemoryTransport<Sink: CommandSink> {
    dispatcher: Dispatcher<Sink>,
    running: bool,
}

impl<Sink: CommandSink> InMemoryTransport<Sink> {
    fn new(dispatcher: Dispatcher<Sink>) -> Self {
        Self {
            dispatcher,
            running: false,
        }
    }
}

impl<Sink: CommandSink + Send> ClientTransport for InMemoryTransport<Sink> {
    fn connect(&mut self) -> Result<(), ClientError> {
        Ok(())
    }

    fn set_tick_rate(&mut self, tick_rate: u64) -> Result<(), ClientError> {
        self.dispatcher.set_tick_rate(tick_rate);
        Ok(())
    }

    fn enqueue_command(&mut self, timestamp: u64, command: String) -> Result<(), ClientError> {
        self.dispatcher.enqueue(timestamp, command);
        if self.running {
            self.dispatcher.advance_to(timestamp);
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), ClientError> {
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), ClientError> {
        self.running = false;
        Ok(())
    }
}

/// Transport that reads commands from a buffered pipe-like source.
struct PipeTransport<Sink: CommandSink> {
    dispatcher: Dispatcher<Sink>,
    reader: Box<dyn BufRead + Send>,
    running: bool,
}

impl<Sink: CommandSink> PipeTransport<Sink> {
    fn new(dispatcher: Dispatcher<Sink>, reader: Box<dyn BufRead + Send>) -> Self {
        Self {
            dispatcher,
            reader,
            running: false,
        }
    }
}

impl<Sink: CommandSink + Send> ClientTransport for PipeTransport<Sink> {
    fn connect(&mut self) -> Result<(), ClientError> {
        Ok(())
    }

    fn set_tick_rate(&mut self, tick_rate: u64) -> Result<(), ClientError> {
        self.dispatcher.set_tick_rate(tick_rate);
        Ok(())
    }

    fn enqueue_command(&mut self, timestamp: u64, command: String) -> Result<(), ClientError> {
        self.dispatcher.enqueue(timestamp, command);
        if self.running {
            self.dispatcher.advance_to(timestamp);
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), ClientError> {
        self.running = true;
        let mut line = String::new();

        while self.running {
            line.clear();
            let bytes = self.reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }

            if line.trim().is_empty() {
                continue;
            }

            self.dispatcher
                .enqueue_from_pipe(line.trim_end_matches(&['\r', '\n'][..]))?;
            self.dispatcher.tick();
        }

        Ok(())
    }

    fn stop(&mut self) -> Result<(), ClientError> {
        self.running = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::ScheduledCommand;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct RecordingSink {
        times: Vec<u64>,
        executed: Vec<ScheduledCommand>,
    }

    impl CommandSink for Arc<Mutex<RecordingSink>> {
        fn publish_time(&mut self, time: u64) {
            if let Ok(mut inner) = self.lock() {
                inner.times.push(time);
            }
        }

        fn execute(&mut self, command: &ScheduledCommand) {
            if let Ok(mut inner) = self.lock() {
                inner.executed.push(command.clone());
            }
        }
    }

    #[test]
    fn in_memory_transport_executes_when_running() {
        let sink = Arc::new(Mutex::new(RecordingSink::default()));
        let dispatcher = Dispatcher::new(sink.clone());
        let mut client = SimulationClient::from_dispatcher(dispatcher);

        client.connect().unwrap();
        client.start().unwrap();
        client
            .enqueue_command(3, "fire".to_string())
            .expect("enqueue should succeed");
        client.stop().unwrap();

        let guard = sink.lock().unwrap();
        assert_eq!(guard.times, vec![3]);
        assert_eq!(guard.executed.len(), 1);
        assert_eq!(guard.executed[0].command, "fire");
    }

    #[test]
    fn pipe_transport_consumes_buffered_lines() {
        let sink = Arc::new(Mutex::new(RecordingSink::default()));
        let dispatcher = Dispatcher::new(sink.clone());
        let input = Cursor::new("1:first\n2:second\n");
        let reader = io::BufReader::new(input);
        let mut client = SimulationClient::from_pipe_reader(dispatcher, reader);

        client.connect().unwrap();
        client.start().unwrap();

        let guard = sink.lock().unwrap();
        assert_eq!(guard.times, vec![1, 2]);
        assert_eq!(guard.executed.len(), 2);
        assert_eq!(guard.executed[0].command, "first");
    }
}
