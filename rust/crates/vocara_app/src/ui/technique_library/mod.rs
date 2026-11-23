//! The technique library — a flat, tag-organized pool of vocal techniques
//! (`ui/technique_library`): the library page
//! ([`technique_library_screen`]) and, in [`detail`], one technique.

pub mod technique_library_screen;
pub mod view_model;
pub mod widgets;

/// One technique's page, as the router knows it
/// (`widgets::technique_detail_screen`).
pub use widgets::technique_detail_screen as detail;

pub use technique_library_screen::{Message, TechniqueLibraryScreen};
