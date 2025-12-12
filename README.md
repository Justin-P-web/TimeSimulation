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
- `tick`: Advance a single tick using the current tick rate.
- `step <delta>`: Manually advance simulated time by `delta` units.
- `advance <timestamp>`: Jump directly to the provided timestamp.
- `run <ticks>`: Advance the dispatcher for the given number of ticks.
- `rate <n>`: Set a new non-zero tick rate.
- `enqueue <timestamp> <command>`: Schedule a command for a future timestamp.
- `pipe <timestamp:command>`: Parse and enqueue using pipe syntax.
- `now`: Print the current simulated time.
- `help`: Show the built-in help message.
- `quit` or `exit`: Terminate the client.

## Demo binaries
The `demo` crate offers two minimal examples:
- `demo`: On non-Windows targets it queues a `2:demo-command` instruction and a `3:scale-time` instruction (which multiplies the timestamp by four when executing) and runs four ticks to execute both. On Windows it listens for a named pipe client, parses each incoming `timestamp:command` line, and ticks after each enqueue. The Windows listener also accepts control lines to steer ticking without recompiling:
  - `start` / `stop`: Resume or pause ticking after enqueues.
  - `rate:<n>`: Update the dispatcher tick rate to `<n>` (must be non-zero).
  - `pipe:<name>`: Disconnect and start listening on a new named pipe `<name>`.
- `wait-for-start` (Windows only): Buffers incoming `timestamp:command` lines until a `start` message arrives, then ticks after every enqueue.

Run the standard demo from the workspace root:

```bash
cargo run -p demo --
```

When the non-Windows demo runs, it enqueues two commands using pipe syntax:

```
2:demo-command
3:scale-time
```

`scale-time` computes `timestamp * 4`, so the dispatcher prints the scaled value alongside the scheduled timestamp:

```
[sim-time] 0
[sim-time] 1
[execute @2] demo-command
[sim-time] 2
[execute @3] scale-time (scaled timestamp: 12)
```

Run the wait-for-start demo on Windows (MSVC target):

```bash
cargo run -p demo --bin wait-for-start --target x86_64-pc-windows-msvc
```

### Sending commands from PowerShell
Use the following PowerShell snippet to send pipe-formatted commands to the wait-for-start demo:

```powershell
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'timesimulation-wait-for-start', [System.IO.Pipes.PipeDirection]::Out)
$pipe.Connect()
$writer = New-Object System.IO.StreamWriter($pipe)
$writer.AutoFlush = $true

$writer.WriteLine('4:prepare')
$writer.WriteLine('8:execute')
$writer.WriteLine('start')
$writer.WriteLine('12:cleanup')

$writer.Dispose()
$pipe.Dispose()
```

## Pipe helpers
Use `parse_pipe_line` to convert `timestamp:command` strings into `PipeCommand` structs and `read_pipe_commands` to load entire pipe files while stopping on the first parse error. These helpers keep dispatcher inputs deterministic and validated.

## Development
- Build everything: `cargo build`
- Run tests for all crates: `cargo test`

The workspace uses the 2024 edition of Rust and relies on Tokio for asynchronous clients.
