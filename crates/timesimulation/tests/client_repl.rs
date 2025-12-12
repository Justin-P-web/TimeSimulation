use std::io::BufReader;
use std::sync::{Arc, Mutex};

use timesimulation::{ClientError, CommandSink, Dispatcher, ScheduledCommand, SimulationClient};

#[derive(Debug, Default)]
struct RecordingSink {
    times: Vec<u64>,
    executed: Vec<ScheduledCommand>,
}

#[derive(Clone, Debug, Default)]
struct SharedRecordingSink(Arc<Mutex<RecordingSink>>);

impl SharedRecordingSink {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(RecordingSink::default())))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecordingSink> {
        self.0.lock().unwrap()
    }
}

impl CommandSink for SharedRecordingSink {
    fn publish_time(&mut self, time: u64) {
        if let Ok(mut guard) = self.0.lock() {
            guard.times.push(time);
        }
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        if let Ok(mut guard) = self.0.lock() {
            guard.executed.push(command.clone());
        }
    }
}

#[test]
fn connecting_and_starting_dispatcher_executes_enqueued_commands() {
    let sink = SharedRecordingSink::new();
    let dispatcher = Dispatcher::new(sink.clone());
    let mut client = SimulationClient::from_dispatcher(dispatcher);

    client.connect().expect("connect should succeed");

    // Commands enqueued before starting should remain pending until the
    // dispatcher is marked as running.
    client
        .enqueue_command(2, "pending".to_string())
        .expect("enqueue should succeed");
    assert!(sink.lock().times.is_empty());

    client.start().expect("start should succeed");
    client
        .enqueue_command(1, "first".to_string())
        .expect("enqueue after start should succeed");
    client
        .enqueue_command(3, "second".to_string())
        .expect("enqueue after start should succeed");

    let guard = sink.lock();
    assert_eq!(guard.times, vec![1, 3]);
    assert_eq!(
        guard
            .executed
            .iter()
            .map(|cmd| (cmd.timestamp, cmd.command.clone()))
            .collect::<Vec<_>>(),
        vec![
            (1, "first".to_string()),
            (2, "pending".to_string()),
            (3, "second".to_string())
        ],
    );
}

#[test]
fn set_tick_rate_updates_progression_for_pipe_transport() {
    let sink = SharedRecordingSink::new();
    let dispatcher = Dispatcher::new(sink.clone());
    let cursor = std::io::Cursor::new("1:first\n2:second\n");
    let reader = BufReader::new(cursor);
    let mut client = SimulationClient::from_pipe_reader(dispatcher, reader);

    client.connect().expect("connect should succeed");
    client
        .set_tick_rate(3)
        .expect("tick rate update should succeed");
    client.start().expect("start should succeed");

    let guard = sink.lock();
    assert_eq!(guard.times, vec![3, 6]);
    assert_eq!(
        guard
            .executed
            .iter()
            .map(|cmd| (cmd.timestamp, cmd.command.clone()))
            .collect::<Vec<_>>(),
        vec![(1, "first".to_string()), (2, "second".to_string())],
    );
}

#[test]
fn repl_style_enqueues_execute_in_timestamp_order() {
    let sink = SharedRecordingSink::new();
    let dispatcher = Dispatcher::new_with_tick_rate(sink.clone(), 0, 1);
    let mut client = SimulationClient::from_dispatcher(dispatcher);

    client.connect().expect("connect should succeed");
    client.start().expect("start should succeed");

    // Enqueue commands in the same order a REPL user would enter them and
    // validate they execute sequentially as simulated time advances.
    client
        .enqueue_command(1, "first".to_string())
        .expect("enqueue should succeed");
    client
        .enqueue_command(2, "second".to_string())
        .expect("enqueue should succeed");
    client
        .enqueue_command(3, "third".to_string())
        .expect("enqueue should succeed");

    let guard = sink.lock();
    assert_eq!(guard.times, vec![1, 2, 3]);
    assert_eq!(
        guard
            .executed
            .iter()
            .map(|cmd| (cmd.timestamp, cmd.command.clone()))
            .collect::<Vec<_>>(),
        vec![
            (1, "first".to_string()),
            (2, "second".to_string()),
            (3, "third".to_string()),
        ],
    );
}

#[test]
#[should_panic(expected = "tick rate must be greater than zero")]
fn set_tick_rate_rejects_zero() {
    let sink = SharedRecordingSink::new();
    let dispatcher = Dispatcher::new(sink);
    let mut client = SimulationClient::from_dispatcher(dispatcher);

    client.connect().unwrap();
    client.set_tick_rate(0).unwrap();
}

#[test]
fn pipe_transport_propagates_invalid_commands() {
    let sink = SharedRecordingSink::new();
    let dispatcher = Dispatcher::new(sink.clone());
    let cursor = std::io::Cursor::new("invalid-line-without-colon\n");
    let reader = BufReader::new(cursor);
    let mut client = SimulationClient::from_pipe_reader(dispatcher, reader);

    client.connect().expect("connect should succeed");
    let result = client.start();
    assert!(matches!(result, Err(ClientError::Pipe(_))));
    assert!(sink.lock().executed.is_empty());
}
