//! The song practice screen's view models (port of
//! `ui/song_practice_detail/view_model`): the feature controllers the detail
//! view model composes, each plain state with methods and no iced types;
//! their I/O comes back to the screen as effect values (see
//! `rust/PORTING.md`, "View models").

pub mod analysis_cache;
pub mod analysis_source;
pub mod interpretations_controller;
pub mod mixer_controller;
pub mod playback_controller;
