# TimeSimulation

TimeSimulation is a Rust workspace for deterministic, wall-clock-free time simulation. It provides a library for scheduling commands against a simulated clock and binaries for interactive control or demonstrations.

## Workspace layout
- `crates/timesimulation`: Core library exposing the simulated clock, dispatcher, scheduler, and pipe parsing utilities.
- `crates/client`: Interactive Tokio-based REPL that drives a dispatcher with commands and periodic ticks.
- `crates/demo`: Minimal example showing how to wire a dispatcher and feed commands from pipe input (with Windows named-pipe support).
- `crates/pipe-listener`: Auxiliary binary for listening to pipe input on Windows targets.

## Core concepts
- **SimClock**: Deterministic clock that advances only when instructed. There is no dependency on real time, which keeps runs reproducible. 
- **Dispatcher**: Orchestrates simulated time progression, publishes clock updates, and executes commands when their scheduled timestamp is reached.
- **CommandScheduler**: Priority queue storing future commands keyed by timestamp.
- **Pipe format**: Lines formatted as `timestamp:command body` parsed into scheduled commands for ingestion from named pipes or other transports.

## Library usage
Add the workspace to your path and depend on `timesimulation` in another crate, or work directly inside the repository. A typical flow constructs a dispatcher with a custom sink, enqueues commands, and drives time forward:

```rust
use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};

#[derive(Default)]
struct RecordingSink;

impl CommandSink for RecordingSink {
    fn publish_time(&mut self, time: u64) {
        println!("time advanced to {time}");
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        println!("executing {} at {}", command.command, command.timestamp);
    }
}

fn main() {
    let sink = RecordingSink::default();
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, 2);

    dispatcher.enqueue(4, "launch".to_string());
    dispatcher.tick(); // time = 2
    dispatcher.tick(); // time = 4, executes "launch"
}
```

## Interactive client (REPL)
The `client` binary provides a REPL for experimenting with the dispatcher. It supports starting/stopping ticking, adjusting the tick rate, enqueueing commands, and parsing pipe-formatted lines.

Run the client from the workspace root:

```bash
cargo run -p client --
```

Key flags:
- `--tick-rate <u64>`: Simulated time advanced per tick (default: `1`).
- `--interval-ms <u64>`: Milliseconds between ticks while ticking is enabled (default: `1000`).
- `--start-time <u64>`: Initial simulated timestamp (default: `0`).
- `--pipe-name <name>` (Windows only): Named pipe to listen on for pipe-formatted commands.

Available REPL commands:
- `start` / `stop`: Enable or pause ticking on the configured interval.
- `rate <n>`: Set a new non-zero tick rate.
- `enqueue <timestamp> <command>`: Schedule a command for a future timestamp.
- `pipe <timestamp:command>`: Parse and enqueue using pipe syntax.
- `now`: Print the current simulated time.
- `help`: Show the built-in help message.
- `quit` or `exit`: Terminate the client.

## Demo binary
The `demo` crate offers a minimal example. On non-Windows targets it queues a single `2:demo-command` instruction and runs two ticks to execute it. On Windows it listens for a named pipe client, parses each incoming `timestamp:command` line, and ticks after each enqueue.

Run the demo from the workspace root:

```bash
cargo run -p demo --
```

## Pipe helpers
Use `parse_pipe_line` to convert `timestamp:command` strings into `PipeCommand` structs and `read_pipe_commands` to load entire pipe files while stopping on the first parse error. These helpers keep dispatcher inputs deterministic and validated.

## Development
- Build everything: `cargo build`
- Run tests for all crates: `cargo test`

The workspace uses the 2024 edition of Rust and relies on Tokio for asynchronous clients.
