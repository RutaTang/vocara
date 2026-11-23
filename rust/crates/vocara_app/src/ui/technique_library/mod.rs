//! The technique library — a flat, tag-organized pool of vocal techniques
//! (`ui/technique_library`): the library page
//! ([`technique_library_screen`]) and, in [`detail`], one technique.

pub mod detail;
pub mod technique_library_screen;
pub mod view_model;
pub mod widgets;

pub use technique_library_screen::{Message, TechniqueLibraryScreen};
