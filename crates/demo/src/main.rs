//! Example binary entry point.
//!
//! This binary demonstrates constructing a dispatcher and stepping the
//! simulated clock. In real deployments, external processes feed pipe lines to
//! the dispatcher. On Windows targets, this example connects to a named pipe
//! client before driving the dispatcher from pipe input; other targets fall
//! back to a static demo command.

use timesimulation::{CommandSink, Dispatcher, ScheduledCommand};
use tokio::sync::mpsc::UnboundedSender;

#[cfg(windows)]
use anyhow::Result;

const OUTPUT_PIPE_SUFFIX: &str = "-output";

#[derive(Debug, Default)]
struct LoggingSink {
    output: Option<UnboundedSender<String>>,
}

impl LoggingSink {
    fn new(output: Option<UnboundedSender<String>>) -> Self {
        Self { output }
    }

    fn emit(&self, message: impl Into<String>) {
        let message = message.into();
        println!("{message}");

        if let Some(sender) = &self.output {
            let _ = sender.send(message);
        }
    }
}

impl CommandSink for LoggingSink {
    fn publish_time(&mut self, time: u64) {
        self.emit(format!("[sim-time] {}", time));
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
#[tokio::main]
async fn main() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut pipe_name = "timesimulation-demo".to_string();
    let mut ticking = true;

    let tick_rate = 2;

    loop {
        let pipe_path = format!(r"\\.\\pipe\\{pipe_name}");
        let output_pipe_path = output_pipe_path(&pipe_name);

        let (output_sender, output_receiver) = tokio::sync::mpsc::unbounded_channel();
        let sink = LoggingSink::new(Some(output_sender.clone()));
        let mut dispatcher = Dispatcher::new_with_tick_rate(sink, 0, tick_rate);

        let output_task = spawn_output_pipe_writer(&output_pipe_path, output_receiver);

        emit_status(
            &output_sender,
            format!(
                "waiting for named pipe client on {pipe_path} (output on {output_pipe_path})..."
            ),
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .in_buffer_size(16 * 1024)
            .out_buffer_size(16 * 1024)
            .create(&pipe_path)?;

        server.connect().await?;
        emit_status(
            &output_sender,
            "client attached; send 'start', 'stop', 'rate:<n>', 'pipe:<name>', or timestamp commands",
        );

        let reader = BufReader::new(server);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let lower = trimmed.to_ascii_lowercase();

            if lower == "start" {
                ticking = true;
                emit_status(
                    &output_sender,
                    "ticking resumed; enqueueing will trigger ticks",
                );
                continue;
            }

            if lower == "stop" {
                ticking = false;
                emit_status(
                    &output_sender,
                    "ticking paused; commands will queue until 'start'",
                );
                continue;
            }

            if let Some(rest) = lower.strip_prefix("rate:") {
                match rest.parse::<u64>() {
                    Ok(rate) if rate > 0 => {
                        dispatcher.set_tick_rate(rate);
                        emit_status(&output_sender, format!("tick rate updated to {rate}"));
                    }
                    _ => emit_status(
                        &output_sender,
                        format!("invalid rate control command: '{trimmed}'"),
                    ),
                }
                continue;
            }

            if let Some(rest) = lower.strip_prefix("pipe:") {
                let target = rest.trim();
                if target.is_empty() {
                    emit_status(&output_sender, "pipe control requires a name after 'pipe:'");
                    continue;
                }

                pipe_name = target.to_string();
                emit_status(
                    &output_sender,
                    format!("switching to new pipe name: {pipe_name}"),
                );
                output_task.abort();
                break;
            }

            match dispatcher.enqueue_from_pipe(trimmed) {
                Ok(()) => {
                    if ticking {
                        dispatcher.tick();
                    } else {
                        emit_status(&output_sender, format!("queued without ticking: {trimmed}"));
                    }
                }
                Err(err) => emit_status(
                    &output_sender,
                    format!("failed to parse pipe line '{trimmed}': {err}"),
                ),
            }
        }

        emit_status(&output_sender, "pipe listener reset; awaiting next client");
        output_task.abort();
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

fn output_pipe_name(pipe_name: &str) -> String {
    format!("{pipe_name}{OUTPUT_PIPE_SUFFIX}")
}

fn output_pipe_path(pipe_name: &str) -> String {
    format!(r"\\.\\pipe\\{}", output_pipe_name(pipe_name))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn emit_status(output: &UnboundedSender<String>, message: impl Into<String>) {
    let message = message.into();
    println!("{message}");
    let _ = output.send(message);
}

#[cfg(windows)]
fn spawn_output_pipe_writer(
    output_pipe_path: &str,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncWriteExt, BufWriter};
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_path = output_pipe_path.to_string();
    tokio::spawn(async move {
        println!("waiting for output pipe client on {pipe_path} (optional)...");
        let server = match ServerOptions::new()
            .first_pipe_instance(true)
            .in_buffer_size(16 * 1024)
            .out_buffer_size(16 * 1024)
            .create(&pipe_path)
        {
            Ok(server) => server,
            Err(err) => {
                eprintln!("failed to bind output pipe {pipe_path}: {err}");
                return;
            }
        };

        if let Err(err) = server.connect().await {
            eprintln!("failed to accept output pipe client on {pipe_path}: {err}");
            return;
        }

        println!("output pipe client attached on {pipe_path}");

        let mut writer = BufWriter::new(server);

        while let Some(line) = receiver.recv().await {
            if let Err(err) = writer.write_all(format!("{line}\n").as_bytes()).await {
                eprintln!("failed to write to output pipe {pipe_path}: {err}");
                break;
            }

            if let Err(err) = writer.flush().await {
                eprintln!("failed to flush output pipe {pipe_path}: {err}");
                break;
            }
        }

        println!("output pipe writer shutting down for {pipe_path}");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_pipe_derives_from_name() {
        assert_eq!(
            output_pipe_name("timesimulation-demo"),
            "timesimulation-demo-output"
        );
        assert_eq!(
            output_pipe_path("timesimulation-demo"),
            r"\\.\\pipe\\timesimulation-demo-output"
        );
    }

    #[test]
    fn logging_sink_forwards_messages() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let sink = LoggingSink::new(Some(sender));

        sink.emit("test-message");

        assert_eq!(
            receiver.try_recv().expect("message forwarded"),
            "test-message"
        );
    }
}
