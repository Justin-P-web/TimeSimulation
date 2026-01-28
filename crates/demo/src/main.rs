//! Example binary entry point.
//!
//! This binary demonstrates constructing a dispatcher and stepping the
//! simulated clock. In real deployments, external processes feed pipe lines to
//! the dispatcher. On Windows targets, this example connects to a named pipe
//! client before driving the dispatcher from pipe input; other targets fall
//! back to a static demo command.

use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};

#[cfg(windows)]
use timesimulation::windows_pipe::{PipeEvent, parse_control_instruction};

#[cfg(windows)]
use anyhow::Result;

#[cfg(windows)]
use tokio::io::{AsyncWriteExt, BufWriter};

#[derive(Debug, Default)]
struct LoggingSink {
    output: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl LoggingSink {
    fn emit(&self, message: String) {
        if let Some(sender) = &self.output {
            let _ = sender.send(message.clone());
        }

        println!("{message}");
    }
}

impl CommandSink for LoggingSink {
    fn publish_time(&mut self, time: u64) {
        self.emit(format!("[sim-time] {time}"));
    }

    fn execute(&mut self, command: &ScheduledCommand) {
        match command.command.as_str() {
            "scale-time" => {
                let scaled = command.timestamp * 4;
                self.emit(format!(
                    "[execute @{}] {} (scaled timestamp: {})",
                    command.timestamp, command.command, scaled
                ));
            }
            _ => self.emit(format!(
                "[execute @{}] {}",
                command.timestamp, command.command
            )),
        }
    }
}

#[cfg(windows)]
async fn forward_output_to_pipe(
    pipe_path: String,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    loop {
        println!("waiting for output pipe client on {pipe_path}...");
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_path)?;

        server.connect().await?;
        println!("output client attached on {pipe_path}");

        let mut writer = BufWriter::new(server);

        while let Some(line) = receiver.recv().await {
            if let Err(err) = writer.write_all(line.as_bytes()).await {
                eprintln!("failed to write to output pipe: {err}");
                break;
            }

            if let Err(err) = writer.write_all(b"\n").await {
                eprintln!("failed to write newline to output pipe: {err}");
                break;
            }

            if let Err(err) = writer.flush().await {
                eprintln!("failed to flush output pipe: {err}");
                break;
            }
        }

        println!("output client disconnected; resetting output pipe listener");

        if receiver.is_closed() {
            break;
        }
    }

    Ok(())
}

#[cfg(windows)]
#[tokio::main]
async fn main() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut pipe_name = "timesimulation-demo".to_string();
    let output_pipe_name = "timesimulation-demo-output";
    let mut ticking = true;

    let (output_sender, mut output_receiver) = tokio::sync::mpsc::unbounded_channel();
    let sink = LoggingSink {
        output: Some(output_sender),
    };
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    let output_pipe_path = format!(r"\\.\\pipe\\{output_pipe_name}");

    tokio::spawn(async move {
        if let Err(err) = forward_output_to_pipe(output_pipe_path, output_receiver).await {
            eprintln!("output pipe listener failed: {err}");
        }
    });

    loop {
        let pipe_path = format!(r"\\.\\pipe\\{pipe_name}");

        println!("waiting for named pipe client on {pipe_path}...");
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .in_buffer_size(16 * 1024)
            .out_buffer_size(16 * 1024)
            .create(&pipe_path)?;

        server.connect().await?;
        println!(
            "client attached; send dispatcher controls (start/stop/tick/step/advance/run/rate/now) or timestamp commands"
        );

        let reader = BufReader::new(server);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("pipe:") {
                let target = rest.trim();
                if target.is_empty() {
                    eprintln!("pipe control requires a name after 'pipe:'");
                    continue;
                }

                pipe_name = target.to_string();
                println!("switching to new pipe name: {pipe_name}");
                break;
            }

            match parse_control_instruction(trimmed) {
                Ok(Some(PipeEvent::Start)) => {
                    ticking = true;
                    println!("ticking resumed; enqueueing will trigger ticks");
                    continue;
                }
                Ok(Some(PipeEvent::Stop | PipeEvent::Pause | PipeEvent::Disconnected)) => {
                    ticking = false;
                    println!("ticking paused; commands will queue until 'start'");
                    continue;
                }
                Ok(Some(PipeEvent::Tick)) => {
                    dispatcher.tick();
                    println!("executed single tick");
                    continue;
                }
                Ok(Some(PipeEvent::Step(delta))) => {
                    dispatcher.step(delta);
                    println!("advanced clock by {delta}");
                    continue;
                }
                Ok(Some(PipeEvent::Advance(target))) => {
                    dispatcher.advance_to(target);
                    println!("advanced clock to {target}");
                    continue;
                }
                Ok(Some(PipeEvent::RunTicks(ticks))) => {
                    dispatcher.run_for_ticks(ticks);
                    println!("ran {ticks} tick(s)");
                    continue;
                }
                Ok(Some(PipeEvent::SetRate(rate))) => {
                    dispatcher.set_tick_rate(rate);
                    println!("tick rate updated to {rate}");
                    continue;
                }
                Ok(Some(PipeEvent::Now)) => {
                    println!("[sim-time] {}", dispatcher.now());
                    continue;
                }
                Ok(Some(PipeEvent::AddFileNote { file, note })) => {
                    println!("received file note for {file}: {note}");
                    continue;
                }
                Ok(Some(PipeEvent::Scheduled(_))) => {
                    // Forward scheduling commands through the standard pipe parser below.
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("failed to parse control instruction '{trimmed}': {err}");
                    continue;
                }
            }

            match dispatcher.enqueue_from_pipe(trimmed) {
                Ok(()) => {
                    if ticking {
                        dispatcher.tick();
                    } else {
                        println!("queued without ticking: {trimmed}");
                    }
                }
                Err(err) => eprintln!("failed to parse pipe line '{trimmed}': {err}"),
            }
        }

        println!("pipe listener reset; awaiting next client");
    }
}

#[cfg(not(windows))]
fn main() {
    let sink = LoggingSink::default();
    let tick_rate = 2;
    let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

    dispatcher
        .enqueue_from_pipe("2:demo-command")
        .expect("demo command should parse");

    dispatcher
        .enqueue_from_pipe("3:scale-time")
        .expect("scale-time command should parse");

    dispatcher.run_for_ticks(4);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_sink_forwards_messages_to_channel() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = LoggingSink {
            output: Some(sender),
        };

        sink.publish_time(5);
        sink.execute(&ScheduledCommand {
            timestamp: 3,
            command: "demo".to_string(),
        });

        let first = receiver
            .try_recv()
            .expect("publish_time should emit a message");
        assert_eq!(first, "[sim-time] 5");

        let second = receiver.try_recv().expect("execute should emit a message");
        assert_eq!(second, "[execute @3] demo");
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use tokio::io::AsyncBufReadExt;
        use tokio::net::windows::named_pipe::ClientOptions;

        #[tokio::test]
        async fn output_forwarder_streams_lines_over_named_pipe() {
            let pipe_path = format!(
                r"\\.\\pipe\\timesimulation-output-test-{}",
                std::process::id()
            );

            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

            let forwarder = tokio::spawn(forward_output_to_pipe(pipe_path.clone(), receiver));

            let client = ClientOptions::new()
                .open(&pipe_path)
                .expect("client should connect to output pipe");

            let mut reader = tokio::io::BufReader::new(client);
            sender
                .send("[sim-time] 1".to_string())
                .expect("first message should send");
            sender
                .send("[execute @2] demo".to_string())
                .expect("second message should send");
            drop(sender);

            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .expect("should read first line");
            assert_eq!(line.trim_end(), "[sim-time] 1");

            line.clear();
            reader
                .read_line(&mut line)
                .await
                .expect("should read second line");
            assert_eq!(line.trim_end(), "[execute @2] demo");

            forwarder
                .await
                .expect("task join should succeed")
                .expect("forwarder should complete successfully");
        }
    }
}
