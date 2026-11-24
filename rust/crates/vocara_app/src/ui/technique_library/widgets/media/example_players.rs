//! The example players of one technique page, kept the way Flutter kept
//! them: one player per example card, KEYED by `(media id, whether its
//! file is missing)` (`technique_media_section.dart` keyed each player
//! `ValueKey((m.id, AssetStore.assetIdOf(m.path) != null))`).
//!
//! What the keying meant, and what [`ExamplePlayers::sync`] keeps:
//!
//! - a card's player lives as long as its key is shown: the page scrolling
//!   it out of view does not drop it (iced lists are not lazy; the state is
//!   the page's, not the view's), and removing an EARLIER example does not
//!   hand its slot to the next one (players are found by key, not
//!   position);
//! - a clip whose file arrives in a reload changes key, so it gets a fresh
//!   player (the old one read "not on this device" once, at creation);
//! - a key no longer shown drops its player (disposes it).
//!
//! # How the technique page owns and polls them
//!
//! The page state holds one [`ExamplePlayers`] and, on every change to the
//! examples it shows (load, reload, edit draft), calls
//! [`ExamplePlayers::sync`] with them and the app's
//! [`MediaPlayerFactory`]. Its view embeds [`ExamplePlayers::view`] for
//! each card, mapped into the page's message (`Message::Examples(..)`); its
//! `update` hands those messages to [`ExamplePlayers::update`] with
//! `play_blocked` (the view model's `micLive`) and the analyzed example's
//! id, and forwards the returned position to `setPlayheadFromPlayback`. Its
//! `subscription` includes [`ExamplePlayers::subscription`] (a tick only
//! while some player plays, loads or settles). The view model's signals
//! become calls: `micArmed` → [`ExamplePlayers::pause_all`],
//! `analysisScrub` → [`ExamplePlayers::seek_analyzed`]. Everything runs on
//! the UI thread in `update`, as the players require; the page drops them
//! (and so stops them) when it is disposed.
//!
//! # The video controls' keyboard and fullscreen
//!
//! A video's controls took the keyboard as they appeared (their
//! `Focus(autofocus: true)`), unless the page's focus scope had a focused
//! node already — the controls of a video started earlier. So the set
//! keeps which video holds the keyboard ([`ExamplePlayers::keyboard_holder`]):
//! the first one started, until it goes, or until a take blocks the
//! examples (`ExcludeFocus` took the focus away, and nothing gave it back:
//! [`ExamplePlayers::set_play_blocked`]). The page wraps its content in
//! [`ExamplePlayers::shortcuts`], which hands that video every key no
//! widget of the page took (a focused text field takes its keys first, as
//! it held the focus). A video in fullscreen is shown by the page over the
//! whole window ([`ExamplePlayers::overlay`]) and takes its own keys.

use std::time::Duration;

use iced::{Element, Event, Subscription};
use vocara_services::media_player::MediaPlayerFactory;

use super::technique_audio_player::{
    self as audio, is_unavailable_path, TechniqueAudioPlayer, AUDIO_POLL_INTERVAL,
};
use super::technique_video_player::{
    self as video, TechniqueVideoPlayer, WindowRequest, VIDEO_POLL_INTERVAL,
};
use crate::ui::core::widgets::wrap::Wrap;

/// One example clip as the page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExampleClip<'a> {
    pub id: &'a str,
    pub path: &'a str,
    pub video: bool,
}

/// What a player is kept by: the example, whether its file is missing on
/// this device, and its kind (Flutter rebuilt a widget of another type).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExampleKey {
    pub id: String,
    pub unavailable: bool,
    pub video: bool,
}

impl ExampleKey {
    pub fn of(clip: &ExampleClip<'_>) -> ExampleKey {
        ExampleKey {
            id: clip.id.to_string(),
            unavailable: is_unavailable_path(clip.path),
            video: clip.video,
        }
    }
}

/// One card's player.
#[derive(Debug)]
pub enum ExamplePlayer {
    Audio(TechniqueAudioPlayer),
    Video(TechniqueVideoPlayer),
}

impl ExamplePlayer {
    fn needs_tick(&self) -> bool {
        match self {
            ExamplePlayer::Audio(p) => p.needs_tick(),
            ExamplePlayer::Video(p) => p.needs_tick(),
        }
    }

    fn pause_signal(&mut self) {
        match self {
            ExamplePlayer::Audio(p) => p.pause_signal(),
            ExamplePlayer::Video(p) => p.pause_signal(),
        }
    }
}

/// The players' messages, addressed by key: a message from a player that
/// has since been replaced reaches nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Audio(ExampleKey, audio::Message),
    Video(ExampleKey, video::Message),
    /// The poll of every player that needs one.
    Tick,
}

/// What a player's message asks of the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Report {
    /// The analyzed example's audio moved here (`onPosition`).
    Position(Duration),
    /// A video entered or left fullscreen: the window's native fullscreen.
    Window(WindowRequest),
}

/// The page's example players, in the order shown (see the module docs).
#[derive(Debug, Default)]
pub struct ExamplePlayers {
    entries: Vec<(ExampleKey, ExamplePlayer)>,
    /// The video whose controls hold the keyboard.
    keyboard: Option<ExampleKey>,
}

impl ExamplePlayers {
    pub fn new() -> ExamplePlayers {
        ExamplePlayers::default()
    }

    /// How many players are kept.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The player shown for example `id`.
    pub fn get(&self, id: &str) -> Option<&ExamplePlayer> {
        self.entries
            .iter()
            .find(|(k, _)| k.id == id)
            .map(|(_, p)| p)
    }

    /// Reconciles with the examples now shown: keeps every player whose key
    /// is still shown (in the new order), builds one for each new key (an
    /// audio clip opens now; a video waits for its first play), and drops
    /// the rest.
    pub fn sync<'a>(
        &mut self,
        clips: impl IntoIterator<Item = ExampleClip<'a>>,
        players: &dyn MediaPlayerFactory,
    ) {
        let mut old = std::mem::take(&mut self.entries);
        for clip in clips {
            let key = ExampleKey::of(&clip);
            if self.entries.iter().any(|(k, _)| *k == key) {
                continue; // a duplicate id: Flutter would have thrown
            }
            let player = match old.iter().position(|(k, _)| *k == key) {
                Some(i) => old.swap_remove(i).1,
                None if clip.video => ExamplePlayer::Video(TechniqueVideoPlayer::new(clip.path)),
                None => ExamplePlayer::Audio(TechniqueAudioPlayer::new(clip.path, players)),
            };
            self.entries.push((key, player));
        }
        // `old` now holds only the players no longer shown: dropped here
        // (and the keyboard with a video that goes).
        if let Some(holder) = &self.keyboard {
            if !self.entries.iter().any(|(k, _)| k == holder) {
                self.keyboard = None;
            }
        }
    }

    /// The video whose controls hold the keyboard, if any.
    pub fn keyboard_holder(&self) -> Option<&ExampleKey> {
        self.keyboard.as_ref()
    }

    /// Whether a take blocks the examples (`playBlocked`): when it does,
    /// the controls lose the keyboard for good (`ExcludeFocus`).
    pub fn set_play_blocked(&mut self, blocked: bool) {
        if blocked {
            self.keyboard = None;
        }
    }

    /// Handles a player's message (or the tick). `play_blocked`: a take is
    /// arming or recording. `analyzed`: the example whose position drives
    /// the analysis (only its audio player reports). Returns the position to
    /// move the analysis playhead to, or what the window must do, if
    /// anything.
    pub fn update(
        &mut self,
        message: Message,
        players: &dyn MediaPlayerFactory,
        play_blocked: bool,
        analyzed: Option<&str>,
    ) -> Option<Report> {
        match message {
            Message::Audio(key, m) => {
                let reports = analyzed == Some(key.id.as_str());
                match self.entry_mut(&key)? {
                    ExamplePlayer::Audio(p) => p
                        .update(m, play_blocked)
                        .filter(|_| reports)
                        .map(Report::Position),
                    ExamplePlayer::Video(_) => None,
                }
            }
            Message::Video(key, m) => {
                let ExamplePlayer::Video(p) = self.entry_mut(&key)? else {
                    return None;
                };
                let had_player = p.has_player();
                let request = p.update(m, players, play_blocked);
                let started = !had_player && p.has_player();
                // The controls appeared: their autofocus takes the
                // keyboard unless another video's controls hold it.
                if started && self.keyboard.is_none() {
                    self.keyboard = Some(key);
                }
                request.map(Report::Window)
            }
            Message::Tick => {
                let mut report = None;
                for (key, player) in &mut self.entries {
                    if !player.needs_tick() {
                        continue;
                    }
                    match player {
                        ExamplePlayer::Audio(p) => {
                            let at = p.update(audio::Message::Tick, play_blocked);
                            if analyzed == Some(key.id.as_str()) {
                                report = at.or(report);
                            }
                        }
                        ExamplePlayer::Video(p) => {
                            let _ = p.update(video::Message::Tick, players, play_blocked);
                        }
                    }
                }
                report.map(Report::Position)
            }
        }
    }

    /// The video whose controls hold the keyboard, while they can take it:
    /// started, inline (a fullscreen video takes its keys in its own view)
    /// and not blocked.
    fn keyboard_video(&self, play_blocked: bool) -> Option<&ExampleKey> {
        let key = self.keyboard.as_ref().filter(|_| !play_blocked)?;
        match self.entries.iter().find(|(k, _)| k == key)? {
            (_, ExamplePlayer::Video(p)) if p.has_player() && !p.is_fullscreen() => Some(key),
            _ => None,
        }
    }

    /// `content` (the page) handing the keys none of its widgets took to
    /// the controls that hold the keyboard (their `CallbackShortcuts`),
    /// as `wrap`ped messages; a key a shortcut takes goes no further.
    pub fn shortcuts<'a, M: 'a>(
        &self,
        content: impl Into<Element<'a, M>>,
        play_blocked: bool,
        wrap: impl Fn(Message) -> M + 'a,
    ) -> Element<'a, M> {
        let holder = self.keyboard_video(play_blocked).cloned();
        Wrap::<M, ()>::new(
            content,
            Box::new(move |_, event, _, _, shell| {
                let (Some(key), Event::Keyboard(event)) = (&holder, event) else {
                    return;
                };
                if shell.is_event_captured() {
                    return;
                }
                if let Some(m) = video::key_message(event) {
                    shell.publish(wrap(Message::Video(key.clone(), m)));
                    shell.capture_event();
                }
            }),
        )
        .into()
    }

    /// The fullscreen view of the video in fullscreen, if one is: shown
    /// over the whole window.
    pub fn overlay(&self) -> Option<Element<'_, Message>> {
        self.entries.iter().find_map(|(key, player)| match player {
            ExamplePlayer::Video(p) => p.fullscreen_view().map(|view| {
                let key = key.clone();
                view.map(move |m| Message::Video(key.clone(), m))
            }),
            ExamplePlayer::Audio(_) => None,
        })
    }

    fn entry_mut(&mut self, key: &ExampleKey) -> Option<&mut ExamplePlayer> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p)
    }

    /// `micArmed`: a take started arming; every example pauses (they play
    /// outside the engine, whose echo canceller cannot subtract them).
    pub fn pause_all(&mut self) {
        for (_, player) in &mut self.entries {
            player.pause_signal();
        }
    }

    /// `analysisScrub`: the analysis was scrubbed to `to`; the analyzed
    /// example's audio player seeks to match.
    pub fn seek_analyzed(&mut self, analyzed: &str, to: Duration) {
        for (key, player) in &mut self.entries {
            if key.id == analyzed {
                if let ExamplePlayer::Audio(p) = player {
                    p.seek_signal(to);
                }
            }
        }
    }

    /// Whether any player needs polling now.
    pub fn needs_tick(&self) -> bool {
        self.entries.iter().any(|(_, p)| p.needs_tick())
    }

    /// The poll: at video rate while a video needs it, else at audio rate
    /// while an audio player does, else nothing.
    pub fn subscription(&self) -> Subscription<Message> {
        let ticking = |video: bool| {
            self.entries
                .iter()
                .any(|(_, p)| p.needs_tick() && matches!(p, ExamplePlayer::Video(_)) == video)
        };
        if ticking(true) {
            iced::time::every(VIDEO_POLL_INTERVAL).map(|_| Message::Tick)
        } else if ticking(false) {
            iced::time::every(AUDIO_POLL_INTERVAL).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// The player of example `id`, for its card.
    pub fn view(&self, id: &str, play_blocked: bool) -> Option<Element<'_, Message>> {
        let (key, player) = self.entries.iter().find(|(k, _)| k.id == id)?;
        Some(match player {
            ExamplePlayer::Audio(p) => {
                let key = key.clone();
                p.view(play_blocked)
                    .map(move |m| Message::Audio(key.clone(), m))
            }
            ExamplePlayer::Video(p) => {
                let key = key.clone();
                p.view(play_blocked)
                    .map(move |m| Message::Video(key.clone(), m))
            }
        })
    }
}

#[cfg(test)]
mod tests;
