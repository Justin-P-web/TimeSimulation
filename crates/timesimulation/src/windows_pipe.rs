//! Asynchronous helpers for consuming commands from Windows named pipes.
//!
//! The routines in this module are only available on Windows targets because
//! they rely on [`tokio::windows::named_pipe`]. Callers should gate usage with
//! conditional compilation when building cross-platform binaries.

use crate::scheduler::ScheduledCommand;
use std::io;
use tokio::sync::mpsc::Sender;

/// Events emitted by the pipe listener.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PipeEvent {
    /// Resume ticking on the configured interval.
    Start,
    /// Stop ticking immediately.
    Stop,
    /// Execute a single tick immediately.
    Tick,
    /// Advance by an explicit delta without changing the tick rate.
    Step(u64),
    /// Jump directly to a target timestamp.
    Advance(u64),
    /// Run the dispatcher for a fixed number of ticks.
    RunTicks(u64),
    /// Update the tick rate used when progressing simulated time.
    SetRate(u64),
    /// Query the current simulated time.
    Now,
    /// Pause ticking without terminating the listener.
    Pause,
    /// Scheduled command to enqueue in the dispatcher.
    Scheduled(ScheduledCommand),
    /// Notification that the remote client disconnected.
    Disconnected,
}

impl From<ScheduledCommand> for PipeEvent {
    fn from(command: ScheduledCommand) -> Self {
        PipeEvent::Scheduled(command)
    }
}

#[cfg(windows)]
use crate::pipe::parse_pipe_line;
#[cfg(windows)]
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use serde_json;
#[cfg(windows)]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(windows)]
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;

#[cfg(windows)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum JsonControlInstruction {
    Start,
    Stop,
    Pause,
    Tick,
    Run { ticks: u64 },
    Step { delta: u64 },
    Advance { target: u64 },
    Now,
    Rate { rate: u64 },
}

#[cfg(windows)]
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[cfg(windows)]
#[derive(Debug)]
struct JsonRpcErrorDetail {
    code: i64,
    message: String,
    data: Option<serde_json::Value>,
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
struct JsonRpcErrorResponse {
    jsonrpc: &'static str,
    id: serde_json::Value,
    error: JsonRpcError,
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
struct JsonRpcSuccessResponse<'a> {
    jsonrpc: &'static str,
    id: serde_json::Value,
    result: &'a str,
}

#[cfg(windows)]
enum ControlParseResult {
    Event(Option<PipeEvent>),
    JsonRpc { id: Option<serde_json::Value>, event: PipeEvent },
}

#[cfg(windows)]
enum ControlParseError {
    JsonRpc(JsonRpcErrorDetail, Option<serde_json::Value>),
    Other(String),
}

/// Spawns a listener that accepts pipe clients and forwards parsed commands to
/// the provided scheduler channel.
///
/// # Errors
/// Returns an [`io::Error`] if the named pipe cannot be bound. Per-client
/// parsing errors are logged and skipped without terminating the listener.
#[cfg(windows)]
pub async fn listen_for_pipe_commands(
    pipe_name: &str,
    sender: Sender<PipeEvent>,
) -> io::Result<()> {
    let pipe_path = format!(r"\\.\\pipe\\{}", pipe_name);

    loop {
        let mut connection = ServerOptions::new().create(&pipe_path)?;
        connection.connect().await?;
        let mut client_sender = sender.clone();

        tokio::spawn(async move {
            if let Err(err) =
                forward_commands(&mut connection, &mut connection, &mut client_sender).await
            {
                eprintln!("named pipe client error: {err}");
            }
        });
    }
}

#[cfg(windows)]
/// Reads newline-delimited control and scheduling instructions from a named
/// pipe client and forwards parsed events to the dispatcher channel.
///
/// The reader is consumed until it disconnects or the receiver channel is
/// closed. Invalid control instructions and malformed scheduler entries are
/// logged but do not terminate the loop, ensuring later well-formed commands
/// still flow through. When the client disconnects a `Disconnected` event is
/// emitted.
///
/// # Errors
/// Returns an [`io::Error`] if reading from the pipe fails or yields an error
/// while awaiting the next line. Errors sending on the channel are treated as a
/// signal that the receiver has shut down and end processing without
/// propagating an error.
#[cfg(windows)]
async fn forward_commands<R, W>(
    reader: &mut R,
    writer: &mut W,
    sender: &mut Sender<PipeEvent>,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    loop {
        match lines.next_line().await? {
            Some(line) => {
                let trimmed = line.trim();
                match parse_control_instruction(trimmed) {
                    Ok(ControlParseResult::Event(Some(event))) => {
                        if sender.send(event).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Ok(ControlParseResult::Event(None)) => {}
                    Ok(ControlParseResult::JsonRpc { id, event }) => {
                        if sender.send(event).await.is_err() {
                            break;
                        }
                        if let Some(id) = id {
                            let response = JsonRpcSuccessResponse {
                                jsonrpc: "2.0",
                                id,
                                result: "ok",
                            };
                            write_jsonrpc_response(writer, response).await?;
                        }
                        continue;
                    }
                    Err(ControlParseError::JsonRpc(detail, id)) => {
                        let response = JsonRpcErrorResponse {
                            jsonrpc: "2.0",
                            id: id.unwrap_or(serde_json::Value::Null),
                            error: JsonRpcError {
                                code: detail.code,
                                message: detail.message,
                                data: detail.data,
                            },
                        };
                        write_jsonrpc_response(writer, response).await?;
                        continue;
                    }
                    Err(ControlParseError::Other(err)) => {
                        eprintln!("failed to parse control instruction '{line}': {err}");
                        continue;
                    }
                }

                match parse_pipe_line(trimmed) {
                    Ok(parsed) => {
                        if sender
                            .send(PipeEvent::Scheduled(ScheduledCommand {
                                timestamp: parsed.timestamp,
                                command: parsed.command,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        eprintln!("failed to parse pipe line '{line}': {err}");
                    }
                }
            }
            None => {
                let _ = sender.send(PipeEvent::Disconnected).await;
                return Ok(());
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
async fn write_jsonrpc_response<W, T>(writer: &mut W, response: T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_string(&response)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(windows)]
/// Attempts to parse a control instruction emitted by a Windows pipe client.
///
/// The parser first accepts JSON-RPC requests (e.g.
/// `{ "jsonrpc": "2.0", "method": "start" }`), then JSON control objects (e.g.
/// `{ "type": "start" }`), and finally falls back to a space- or
/// colon-delimited shorthand such as `rate 2` or `tick`. Unknown commands
/// produce `ControlParseResult::Event(None)` so the caller can treat the input
/// as a scheduler line instead. Numeric parameters must fit in `u64`; rate
/// updates are validated to reject zeros to prevent a stalled dispatcher.
///
/// # Errors
/// Returns a formatted usage error when numeric parsing fails or when a zero
/// tick rate is provided. Malformed JSON-RPC requests return a structured error
/// and do not fall back to other syntaxes.
#[cfg(windows)]
pub fn parse_control_instruction(line: &str) -> Result<ControlParseResult, ControlParseError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
        if value.get("jsonrpc").is_some() {
            let request: JsonRpcRequest = serde_json::from_value(value.clone()).map_err(|err| {
                ControlParseError::JsonRpc(
                    JsonRpcErrorDetail {
                        code: -32600,
                        message: format!("invalid JSON-RPC request: {err}"),
                        data: None,
                    },
                    value.get("id").cloned(),
                )
            })?;
            let event = parse_jsonrpc_request(&request)?;
            return Ok(ControlParseResult::JsonRpc {
                id: request.id,
                event,
            });
        }
    }

    if let Ok(instruction) = serde_json::from_str::<JsonControlInstruction>(line) {
        let event = match instruction {
            JsonControlInstruction::Start => PipeEvent::Start,
            JsonControlInstruction::Stop => PipeEvent::Stop,
            JsonControlInstruction::Pause => PipeEvent::Pause,
            JsonControlInstruction::Tick => PipeEvent::Tick,
            JsonControlInstruction::Run { ticks } => PipeEvent::RunTicks(ticks),
            JsonControlInstruction::Step { delta } => PipeEvent::Step(delta),
            JsonControlInstruction::Advance { target } => PipeEvent::Advance(target),
            JsonControlInstruction::Now => PipeEvent::Now,
            JsonControlInstruction::Rate { rate } => {
                if rate == 0 {
                    return Err(ControlParseError::Other(
                        "tick rate must be greater than zero".to_string(),
                    ));
                }
                PipeEvent::SetRate(rate)
            }
        };

        return Ok(ControlParseResult::Event(Some(event)));
    }

    let (command, rest) = match line.splitn(2, ' ').collect::<Vec<_>>().as_slice() {
        [command, rest] => (command.to_ascii_lowercase(), rest.trim()),
        [command] => match command.split_once(':') {
            Some((cmd, rest)) => (cmd.to_ascii_lowercase(), rest.trim()),
            None => (command.to_ascii_lowercase(), ""),
        },
        _ => (String::new(), ""),
    };

    let parsed = match command.as_str() {
        "start" => Some(PipeEvent::Start),
        "stop" => Some(PipeEvent::Stop),
        "pause" => Some(PipeEvent::Pause),
        "tick" => Some(PipeEvent::Tick),
        "step" => {
            let delta = rest.parse::<u64>().map_err(|_| {
                ControlParseError::Other("usage: step <non-negative delta>".to_string())
            })?;
            Some(PipeEvent::Step(delta))
        }
        "advance" => {
            let target = rest.parse::<u64>().map_err(|_| {
                ControlParseError::Other("usage: advance <target timestamp>".to_string())
            })?;
            Some(PipeEvent::Advance(target))
        }
        "run" => {
            let ticks = rest.parse::<u64>().map_err(|_| {
                ControlParseError::Other("usage: run <number of ticks>".to_string())
            })?;
            Some(PipeEvent::RunTicks(ticks))
        }
        "rate" => {
            let rate = rest.parse::<u64>().map_err(|_| {
                ControlParseError::Other("usage: rate <non-zero u64>".to_string())
            })?;
            if rate == 0 {
                return Err(ControlParseError::Other(
                    "tick rate must be greater than zero".to_string(),
                ));
            }
            Some(PipeEvent::SetRate(rate))
        }
        "now" => Some(PipeEvent::Now),
        _ => None,
    };

    Ok(ControlParseResult::Event(parsed))
}

#[cfg(windows)]
fn parse_jsonrpc_request(request: &JsonRpcRequest) -> Result<PipeEvent, ControlParseError> {
    let method = request.method.as_str();
    let event = match method {
        "start" => PipeEvent::Start,
        "stop" => PipeEvent::Stop,
        "pause" => PipeEvent::Pause,
        "tick" => PipeEvent::Tick,
        "run" => PipeEvent::RunTicks(extract_jsonrpc_u64(
            &request.params,
            "ticks",
            method,
            &request.id,
        )?),
        "step" => PipeEvent::Step(extract_jsonrpc_u64(
            &request.params,
            "delta",
            method,
            &request.id,
        )?),
        "advance" => PipeEvent::Advance(extract_jsonrpc_u64(
            &request.params,
            "target",
            method,
            &request.id,
        )?),
        "now" => PipeEvent::Now,
        "rate" => {
            let rate = extract_jsonrpc_u64(&request.params, "rate", method, &request.id)?;
            if rate == 0 {
                return Err(ControlParseError::JsonRpc(
                    JsonRpcErrorDetail {
                        code: -32602,
                        message: "invalid params: tick rate must be greater than zero".to_string(),
                        data: None,
                    },
                    request.id.clone(),
                ));
            }
            PipeEvent::SetRate(rate)
        }
        _ => {
            return Err(ControlParseError::JsonRpc(
                JsonRpcErrorDetail {
                    code: -32601,
                    message: format!("method not found: '{method}'"),
                    data: None,
                },
                request.id.clone(),
            ))
        }
    };

    Ok(event)
}

#[cfg(windows)]
fn extract_jsonrpc_u64(
    params: &Option<serde_json::Value>,
    field: &str,
    method: &str,
    id: &Option<serde_json::Value>,
) -> Result<u64, ControlParseError> {
    let value = match params {
        Some(serde_json::Value::Number(number)) => number.as_u64(),
        Some(serde_json::Value::Array(values)) => values.get(0).and_then(|value| value.as_u64()),
        Some(serde_json::Value::Object(map)) => map.get(field).and_then(|value| value.as_u64()),
        _ => None,
    };

    value.ok_or_else(|| {
        ControlParseError::JsonRpc(
            JsonRpcErrorDetail {
                code: -32602,
                message: format!("invalid params: method '{method}' requires '{field}'"),
                data: None,
            },
            id.clone(),
        )
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn parse_control_instruction_supports_json_and_shorthand() {
        // Arrange
        let json_input = "{\"type\":\"rate\",\"rate\":5}";
        let jsonrpc_input = "{\"jsonrpc\":\"2.0\",\"method\":\"rate\",\"params\":{\"rate\":7}}";
        let shorthand_tick = "tick";
        let invalid_rate = "rate 0";

        // Act
        let parsed_json = parse_control_instruction(json_input).unwrap();
        let parsed_jsonrpc = parse_control_instruction(jsonrpc_input).unwrap();
        let parsed_tick = parse_control_instruction(shorthand_tick).unwrap();
        let rate_error = parse_control_instruction(invalid_rate).unwrap_err();

        // Assert
        assert!(matches!(
            parsed_json,
            ControlParseResult::Event(Some(PipeEvent::SetRate(5)))
        ));
        assert!(matches!(
            parsed_jsonrpc,
            ControlParseResult::JsonRpc {
                event: PipeEvent::SetRate(7),
                ..
            }
        ));
        assert!(matches!(
            parsed_tick,
            ControlParseResult::Event(Some(PipeEvent::Tick))
        ));
        match rate_error {
            ControlParseError::Other(message) => {
                assert_eq!(message, "tick rate must be greater than zero");
            }
            _ => panic!("expected non-JSON-RPC error"),
        }
    }

    #[test]
    fn parse_control_instruction_rejects_malformed_jsonrpc() {
        // Arrange
        let malformed = "{\"jsonrpc\":\"2.0\",\"method\":\"rate\",\"params\":{}}";

        // Act
        let error = parse_control_instruction(malformed).unwrap_err();

        // Assert
        match error {
            ControlParseError::JsonRpc(detail, _) => {
                assert!(detail.message.contains("invalid params"));
                assert_eq!(detail.code, -32602);
            }
            _ => panic!("expected JSON-RPC error"),
        }
    }

    #[tokio::test]
    async fn forward_commands_emits_control_and_schedule_events() {
        // Arrange
        let (client, server) = tokio::io::duplex(256);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (mut server_reader, mut server_writer) = tokio::io::split(server);
        let (mut sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let forwarder = tokio::spawn(async move {
            forward_commands(&mut server_reader, &mut server_writer, &mut sender).await
        });

        // Act
        client_writer
            .write_all(b"{\"type\":\"start\"}\nstep 3\n4:launch\n")
            .await
            .unwrap();
        client_writer.shutdown().await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
            if events.len() == 3 {
                break;
            }
        }
        forwarder.await.expect("forwarder should finish").unwrap();

        // Assert
        assert_eq!(
            events,
            vec![
                PipeEvent::Start,
                PipeEvent::Step(3),
                PipeEvent::Scheduled(ScheduledCommand {
                    timestamp: 4,
                    command: "launch".to_string(),
                }),
            ]
        );
        let mut response_line = String::new();
        let mut reader = BufReader::new(&mut client_reader);
        let bytes = reader.read_line(&mut response_line).await.unwrap();
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn forward_commands_writes_jsonrpc_responses() {
        // Arrange
        let (client, server) = tokio::io::duplex(256);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (mut server_reader, mut server_writer) = tokio::io::split(server);
        let (mut sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let forwarder = tokio::spawn(async move {
            forward_commands(&mut server_reader, &mut server_writer, &mut sender).await
        });

        // Act
        client_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"start\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"rate\",\"params\":{\"rate\":0}}\n",
            )
            .await
            .unwrap();
        client_writer.shutdown().await.unwrap();

        let mut responses = Vec::new();
        let mut reader = BufReader::new(&mut client_reader);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap() > 0 {
            responses.push(line.trim().to_string());
            line.clear();
            if responses.len() == 2 {
                break;
            }
        }

        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            events.push(event);
            if events.len() == 1 {
                break;
            }
        }
        forwarder.await.expect("forwarder should finish").unwrap();

        // Assert
        assert_eq!(events, vec![PipeEvent::Start]);
        let success: serde_json::Value = serde_json::from_str(&responses[0]).unwrap();
        assert_eq!(success["jsonrpc"], "2.0");
        assert_eq!(success["id"], 1);
        assert_eq!(success["result"], "ok");
        let error: serde_json::Value = serde_json::from_str(&responses[1]).unwrap();
        assert_eq!(error["jsonrpc"], "2.0");
        assert_eq!(error["id"], 2);
        assert_eq!(error["error"]["code"], -32602);
    }
}

/// Stub implementation used on non-Windows targets to keep the crate
/// cross-platform. Calling this function outside Windows returns an error at
/// runtime.
#[cfg(not(windows))]
pub async fn listen_for_pipe_commands(
    _pipe_name: &str,
    _sender: Sender<PipeEvent>,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "named pipes are only available on Windows targets",
    ))
}
