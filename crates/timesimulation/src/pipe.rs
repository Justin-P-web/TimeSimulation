//! Minimal named pipe helpers for ingesting scheduled commands.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use crate::command::CommandPayload;
use crate::scheduler::ScheduledCommand;

/// Represents a parsed command originating from a named pipe.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PipeCommand {
    /// Target timestamp for execution.
    pub timestamp: u64,
    /// Command payload parsed from the pipe.
    pub command: CommandPayload,
}

/// Errors that can occur while parsing or reading named pipe input.
#[derive(Debug, thiserror::Error)]
pub enum PipeParseError {
    /// Timestamp portion was not an unsigned integer.
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),
    /// Input was missing the expected separator.
    #[error("missing separator ':' in instruction")]
    MissingSeparator,
    /// I/O error encountered while reading from the pipe.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Parses a single line from a named pipe into a [`PipeCommand`].
///
/// The expected format is `timestamp:command body`. Leading and trailing
/// whitespace around the timestamp and command body are trimmed. Empty command
/// bodies are permitted and preserve deterministic scheduling.
pub fn parse_pipe_line(line: &str) -> Result<PipeCommand, PipeParseError> {
    let mut parts = line.splitn(2, ':');
    let timestamp_part = parts.next().unwrap_or("").trim();
    let command_part = parts.next().ok_or(PipeParseError::MissingSeparator)?.trim();
    let timestamp = timestamp_part
        .parse::<u64>()
        .map_err(|_| PipeParseError::InvalidTimestamp(timestamp_part.to_string()))?;

    Ok(PipeCommand {
        timestamp,
        command: CommandPayload::raw(command_part),
    })
}

/// Reads commands from the given named pipe path and returns any successfully
/// parsed instructions.
///
/// This helper deliberately stops reading on the first parse error to keep the
/// dispatcher from continuing with ambiguous inputs.
pub fn read_pipe_commands<P: AsRef<Path>>(
    path: P,
) -> Result<Vec<ScheduledCommand>, PipeParseError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut commands = Vec::new();

    for line in reader.lines() {
        let raw_line = line?;
        let parsed = parse_pipe_line(&raw_line)?;
        commands.push(ScheduledCommand {
            timestamp: parsed.timestamp,
            command: parsed.command,
        });
    }

    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_pipe_lines_with_whitespace() {
        let parsed = parse_pipe_line(" 42 : go ").expect("parse should succeed");
        assert_eq!(parsed.timestamp, 42);
        assert_eq!(parsed.command.as_str(), "go");
    }

    #[test]
    fn rejects_missing_separator() {
        let err = parse_pipe_line("42 go").unwrap_err();
        assert!(matches!(err, PipeParseError::MissingSeparator));
    }

    #[test]
    fn reads_commands_from_pipe_file() {
        let mut temp = NamedTempFile::new().expect("temp file should create");
        writeln!(temp, "1:first").unwrap();
        writeln!(temp, "2:second").unwrap();

        let commands = read_pipe_commands(temp.path()).expect("reading should succeed");
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].command.as_str(), "first");
        assert_eq!(commands[1].timestamp, 2);
    }
}
