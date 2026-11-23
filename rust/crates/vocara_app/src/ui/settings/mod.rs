//! Global app settings (`ui/settings`): the settings page
//! ([`settings_screen`]) and the licenses it opens ([`licenses`]).

pub mod licenses;
pub mod settings_screen;

pub use settings_screen::{Message, SettingsScreen};
