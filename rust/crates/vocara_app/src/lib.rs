//! Vocara's iced application: the app shell (window, quit guard, fatal-open
//! flow, router), the shared UI layer (`ui::core`: theme and widgets), the
//! screens, and the UI utilities. See `rust/PORTING.md`, "The iced
//! application", for how a screen plugs in.

pub mod ui;
pub mod util;
