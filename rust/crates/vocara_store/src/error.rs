//! The store's one error type (see PORTING.md, Conventions).
//!
//! Each variant stands for the Dart exception the ported code threw in the
//! same place, so callers can still tell the cases apart.

use crate::sync::hlc::HlcDriftError;

/// Everything the store can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A SQLite failure (Dart: `SqliteException`).
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// A file system failure (Dart: `FileSystemException`).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A row a table refuses before it reaches SQLite: the checks drift ran
    /// in code, such as the 26..=36 character id (Dart:
    /// `InvalidDataException`).
    #[error("invalid data: {0}")]
    InvalidData(String),

    /// A caller passed a value the API refuses (Dart: `ArgumentError`).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// An operation ran in a state that forbids it (Dart: `StateError`).
    #[error("bad state: {0}")]
    State(String),

    /// Text that does not parse (Dart: `FormatException`).
    #[error("format error: {0}")]
    Format(String),

    /// A schema migration failed; the original database is untouched (Dart:
    /// `MigrationException`).
    #[error("MigrationException: {message}{}", cause.as_ref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Migration {
        /// What failed.
        message: String,
        /// The underlying failure, when there was one.
        cause: Option<Box<Error>>,
    },

    /// A remote HLC implausibly far in the future (Dart:
    /// `HlcDriftException`).
    #[error(transparent)]
    HlcDrift(#[from] HlcDriftError),
}

impl Error {
    /// A [`Error::Migration`] with no underlying cause.
    pub fn migration(message: impl Into<String>) -> Self {
        Error::Migration {
            message: message.into(),
            cause: None,
        }
    }
}

/// The store's result type.
pub type Result<T> = std::result::Result<T, Error>;
