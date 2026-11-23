//! The SQLite working store and its file-level safety machinery (ported from
//! `lib/data/db`).

pub mod rows;
pub mod schema;

pub use schema::{AppDatabase, CURRENT_SCHEMA_VERSION};
