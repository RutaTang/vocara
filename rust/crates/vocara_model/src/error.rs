//! The crate's error type.

use crate::util::dart_json::EncodeError;

/// Why a model, a parse or an encode failed: the Rust form of what the Dart
/// code threw.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// A JSON value of the wrong type where Dart's `as` cast throws a
    /// `TypeError` (a required key missing, a string where a number belongs,
    /// …): the stored row is malformed and the load fails, as in Dart.
    #[error("type error: {0}")]
    Type(String),
    /// Dart's `FormatException`: [message], and the [input] it was about.
    #[error("FormatException: {message}")]
    Format {
        message: String,
        input: Option<String>,
    },
    /// A value with no JSON text (NaN or ±infinity), where `jsonEncode`
    /// throws.
    #[error(transparent)]
    Encode(#[from] EncodeError),
}

impl Error {
    /// A `FormatException(message, input)`.
    pub fn format(message: impl Into<String>, input: impl Into<String>) -> Error {
        Error::Format {
            message: message.into(),
            input: Some(input.into()),
        }
    }
}

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;
