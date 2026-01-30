//! Typed command payloads used by the scheduler and dispatcher.

use std::fmt;

/// Represents the payload of a scheduled command.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CommandPayload {
    /// Raw string content, used as a fallback when no typed variant is available.
    Raw(String),
}

impl CommandPayload {
    /// Creates a raw command payload.
    pub fn raw<S: Into<String>>(value: S) -> Self {
        Self::Raw(value.into())
    }

    /// Returns the raw command string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Raw(value) => value,
        }
    }
}

impl fmt::Display for CommandPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(value) => write!(f, "{value}"),
        }
    }
}

impl From<String> for CommandPayload {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}

impl From<&str> for CommandPayload {
    fn from(value: &str) -> Self {
        Self::Raw(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_payload_formats_as_string() {
        let payload = CommandPayload::raw("launch");
        assert_eq!(payload.to_string(), "launch");
        assert_eq!(payload.as_str(), "launch");
    }
}
