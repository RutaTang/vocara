//! The aggregate repositories: the single source of truth for song practices
//! and the technique library (ported from `lib/data/repositories`).
//!
//! Each repository is an interface (a `Send + Sync` trait, so the view models
//! can be tested against fakes) and its SQLite implementation. Every write
//! goes through the [`SyncWriter`](crate::sync::sync_writer::SyncWriter)
//! chokepoint and the per-entity three-way
//! [`reconcile_collection`](crate::sync::reconcile::reconcile_collection),
//! and every save runs under the store write gate's shared side.
//!
//! Dart's `Result<T>` is the crate's [`Result`](crate::Result): a data-layer
//! failure of any kind comes back as an `Err`, never a panic.

mod common;
pub mod song_practice_repository;
pub mod song_practice_repository_drift;
pub mod technique_repository;
pub mod technique_repository_drift;

pub use song_practice_repository::SongPracticeRepository;
pub use song_practice_repository_drift::SongPracticeRepositoryDrift;
pub use technique_repository::TechniqueRepository;
pub use technique_repository_drift::TechniqueRepositoryDrift;

use std::sync::Arc;

/// A test seam run at the start of every `load_practice` / `load_technique`
/// with the id asked for; an `Err` fails that load (Dart's tests subclassed
/// the repository and overrode the load). A save's re-read after its commit
/// is such a load.
pub type DebugLoadHook = Arc<dyn Fn(&str) -> crate::Result<()> + Send + Sync>;
