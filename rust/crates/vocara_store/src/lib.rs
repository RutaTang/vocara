//! Vocara library store: the SQLite working store, the content-addressed asset pool, the sync machinery and the repositories.

mod dart;
pub mod db;
pub mod error;
pub mod sync;

pub use error::{Error, Result};
