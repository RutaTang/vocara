//! A song practice (`ui/song_practice_detail`): the multi-track studio page
//! of one practice ([`SongPracticeDetailScreen`], the `/song/<id>` route),
//! its view models and its widgets.

pub mod view_model;
pub mod widgets;

pub use widgets::song_practice_detail_screen::{Message, SongPracticeDetailScreen};
