//! Home — the collection of song practices (`ui/home`): the view model
//! ([`view_model`]), the screen ([`home_screen`]) and its widgets.

pub mod home_screen;
pub mod view_model;
pub mod widgets;

#[cfg(test)]
pub(crate) mod testing;

pub use home_screen::{HomeScreen, Message};
