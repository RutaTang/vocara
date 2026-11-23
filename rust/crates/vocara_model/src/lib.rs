//! Vocara domain model: practices, takes, FX, techniques, analysis results,
//! and the pure utilities and infrastructure every layer above needs.

pub mod diagnostics;
mod error;
pub mod models;
pub mod util;

pub use error::{Error, Result};
