//! The technique detail view model's tests: ports of the view-model cases
//! of the `client/test/technique_*_test.dart` files that drive
//! `TechniqueDetailViewModel` (one module per Dart file), and the Rust
//! port's own seams (the recorder's ticks, the quit bridge, the jobs).
//!
//! Where a Dart test drove the detail screen's widgets, its view-model
//! assertions are ported here; what it checked on screen (a button's
//! enabled state, a label, a dialog) belongs to the detail screen's port,
//! and each module lists those parts.

pub(crate) mod support;

mod detail_layout;
mod dropped_frames;
mod example_identity;
mod media_pool_guard;
mod mid_edit_refresh;
mod pending_start;
mod port_seams;
mod practice_failure;
mod practice_take;
mod practice_take_generation;
mod quit_finalize;
mod reference_analysis_queue;
mod save_selection;
mod sibling_paths;
