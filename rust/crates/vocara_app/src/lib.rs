//! Vocara's iced application: the app shell (window, quit guard, fatal-open
//! flow, router), the shared UI layer (`ui::core`: theme and widgets), the
//! screens, and the UI utilities. See `rust/PORTING.md`, "The iced
//! application", and the `ui` module docs for how a screen plugs in.

pub mod app;
pub mod router;
pub mod services;
pub mod ui;
pub mod util;
