//! The test player: what the Dart widget tests swapped in through
//! `createExamplePlayer` (a widget test could not initialize media_kit). It
//! records what it is asked to do and whether it was dropped (disposed), and
//! reports whatever state the test gives it.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::{
    clamp_volume, MediaKind, MediaPlayer, MediaPlayerError, MediaPlayerFactory, PlayerState,
    Result, VideoFrame,
};

#[derive(Debug)]
struct Shared {
    kind: MediaKind,
    fail_open: bool,
    opened: Vec<(String, bool)>,
    plays: usize,
    pauses: usize,
    seeks: Vec<Duration>,
    volumes: Vec<f64>,
    disposed: bool,
    state: PlayerState,
    frame: Option<VideoFrame>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fake player. `play` and `pause` flip its `playing` (as media_kit's did
/// at once), `seek` moves its position, `set_volume` and `set_muted` set
/// what it reports; everything else it reports is what the test sets
/// through its [`FakePlayerHandle`].
#[derive(Debug)]
pub struct FakeMediaPlayer {
    shared: Arc<Mutex<Shared>>,
}

/// The test's view of a [`FakeMediaPlayer`]: what it was asked, and the
/// state it should report. Outlives the player.
#[derive(Debug, Clone)]
pub struct FakePlayerHandle {
    shared: Arc<Mutex<Shared>>,
}

impl FakeMediaPlayer {
    /// A fake player of `kind`; when `fail_open`, every `open` fails.
    pub fn new(kind: MediaKind, fail_open: bool) -> (FakeMediaPlayer, FakePlayerHandle) {
        let shared = Arc::new(Mutex::new(Shared {
            kind,
            fail_open,
            opened: Vec::new(),
            plays: 0,
            pauses: 0,
            seeks: Vec::new(),
            volumes: Vec::new(),
            disposed: false,
            state: PlayerState::default(),
            frame: None,
        }));
        (
            FakeMediaPlayer {
                shared: shared.clone(),
            },
            FakePlayerHandle { shared },
        )
    }
}

impl MediaPlayer for FakeMediaPlayer {
    fn open(&mut self, path: &str, play: bool) -> Result<()> {
        let mut s = lock(&self.shared);
        s.opened.push((path.to_string(), play));
        if s.fail_open {
            return Err(MediaPlayerError::Open {
                path: path.to_string(),
                reason: "the fake cannot open clips".to_string(),
            });
        }
        s.state.playing = play;
        Ok(())
    }

    fn play(&mut self) {
        let mut s = lock(&self.shared);
        s.plays += 1;
        s.state.playing = true;
        s.state.completed = false;
    }

    fn pause(&mut self) {
        let mut s = lock(&self.shared);
        s.pauses += 1;
        s.state.playing = false;
    }

    fn seek(&mut self, to: Duration) {
        let mut s = lock(&self.shared);
        s.seeks.push(to);
        s.state.position = to;
    }

    fn set_volume(&mut self, volume: f64) {
        let mut s = lock(&self.shared);
        s.volumes.push(volume);
        s.state.volume = clamp_volume(volume);
    }

    fn set_muted(&mut self, muted: bool) {
        lock(&self.shared).state.muted = muted;
    }

    fn state(&self) -> PlayerState {
        lock(&self.shared).state.clone()
    }

    fn frame(&mut self) -> Option<VideoFrame> {
        lock(&self.shared).frame.clone()
    }
}

impl Drop for FakeMediaPlayer {
    fn drop(&mut self) {
        lock(&self.shared).disposed = true;
    }
}

impl FakePlayerHandle {
    pub fn kind(&self) -> MediaKind {
        lock(&self.shared).kind
    }

    /// Every `open`: the path, and whether it asked to play.
    pub fn opened(&self) -> Vec<(String, bool)> {
        lock(&self.shared).opened.clone()
    }

    /// How many times it was asked to play (directly or by `play_or_pause`).
    pub fn plays(&self) -> usize {
        lock(&self.shared).plays
    }

    /// How many times it was asked to pause (directly or by `play_or_pause`).
    pub fn pauses(&self) -> usize {
        lock(&self.shared).pauses
    }

    /// Every seek target, in order.
    pub fn seeks(&self) -> Vec<Duration> {
        lock(&self.shared).seeks.clone()
    }

    /// Every volume it was asked for, in order (before clamping).
    pub fn volumes(&self) -> Vec<f64> {
        lock(&self.shared).volumes.clone()
    }

    /// Whether the player was dropped (media_kit's `dispose`).
    pub fn disposed(&self) -> bool {
        lock(&self.shared).disposed
    }

    /// The state it reports now.
    pub fn state(&self) -> PlayerState {
        lock(&self.shared).state.clone()
    }

    /// Changes the state it reports — the player starting by itself (a
    /// shortcut, a media key), its position moving, its duration loading.
    pub fn set_state(&self, change: impl FnOnce(&mut PlayerState)) {
        change(&mut lock(&self.shared).state);
    }

    /// The player reports playing (or not), as if started or stopped by
    /// something other than the widget.
    pub fn set_playing(&self, playing: bool) {
        self.set_state(|s| s.playing = playing);
    }

    /// A new video frame: `width * height` RGBA pixels, the next revision.
    pub fn push_frame(&self, width: u32, height: u32, rgba: Vec<u8>) {
        let mut s = lock(&self.shared);
        let revision = s.frame.as_ref().map_or(0, |f| f.revision) + 1;
        s.frame = Some(VideoFrame {
            width,
            height,
            rgba: Arc::new(rgba),
            revision,
        });
    }
}

/// A factory of [`FakeMediaPlayer`]s that keeps a handle to every player it
/// built (the Dart tests' `_fakePlayers()`).
#[derive(Debug, Clone, Default)]
pub struct FakeMediaPlayers {
    built: Arc<Mutex<Vec<FakePlayerHandle>>>,
    fail_open: bool,
}

impl FakeMediaPlayers {
    pub fn new() -> FakeMediaPlayers {
        FakeMediaPlayers::default()
    }

    /// Players whose `open` fails: any failure after a player was built (in
    /// Dart, the fake could not back a `VideoController`).
    pub fn failing_open() -> FakeMediaPlayers {
        FakeMediaPlayers {
            fail_open: true,
            ..FakeMediaPlayers::default()
        }
    }

    /// Every player built so far, oldest first.
    pub fn built(&self) -> Vec<FakePlayerHandle> {
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl MediaPlayerFactory for FakeMediaPlayers {
    fn create(&self, kind: MediaKind) -> Result<Box<dyn MediaPlayer>> {
        let (player, handle) = FakeMediaPlayer::new(kind, self.fail_open);
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
        Ok(Box::new(player))
    }
}
