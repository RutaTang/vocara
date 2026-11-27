//! Port of `ui/technique_library/widgets/media/technique_video_player.dart`:
//! an inline video player for a technique example clip.
//!
//! To avoid spinning up a native player (and a texture) for every clip just
//! by showing the viewer, the player is created LAZILY on the first press of
//! the poster's play button; a lightweight poster shows until then. Once
//! started, the latest frame shows letterboxed in the 16:9 box (black
//! behind it, `Video(fill: Colors.black)`), under media_kit's desktop
//! controls (`video_controls`: the seek bar, play/pause, the volume button
//! and slider, the position, the fullscreen button, the buffering spinner),
//! which show while the pointer moves over the video and hide 3 s after.
//!
//! The box is Dart's: at most 520 wide, its 1 px `Studio.border` rounded
//! 10 (the border insets the video by its width, as a `Container`'s border
//! did), black.
//!
//! # Fullscreen
//!
//! media_kit's fullscreen pushed a route on the root navigator — the same
//! video, with controls of its own (the fullscreen theme, the same values),
//! filling the window over everything, the page toolbar included — and
//! made the window natively fullscreen (`toggleFullScreen:` when it was
//! not; leaving does the reverse when it is). Here the player keeps a
//! `fullscreen` flag: the page shows [`TechniqueVideoPlayer::fullscreen_view`]
//! over the whole window ([`Screen::overlay`](crate::ui::Screen::overlay)),
//! taking every key and every click, and the change asks the window for
//! its native fullscreen ([`WindowRequest`], [`window_task`]: iced's
//! borderless fullscreen, which winit makes with `toggleFullScreen:`). The
//! fullscreen button, F, a double press and (in fullscreen) Escape toggle
//! it, as media_kit's did. A window left native fullscreen another way
//! (the green button) keeps showing the video, as the route stayed.
//!
//! # The keyboard
//!
//! The controls' `Focus(autofocus: true)` took the keyboard when they
//! appeared, unless something on the page held it already — another video
//! started earlier did. The page keeps which video's controls hold it
//! (`example_players`) and hands that video its shortcuts as
//! [`Message::Key`]; the fullscreen view takes its own.
//!
//! # While a take records (`play_blocked`)
//!
//! Dart's `IgnorePointer` + `ExcludeFocus` around the inline video: the
//! poster and the controls take no input (the controls go as if the
//! pointer left), its shortcuts are off and the controls lose the keyboard
//! for good, and anything that starts the player anyway is paused again on
//! the next poll
//! ([`pause_while_play_blocked`](super::technique_audio_player::pause_while_play_blocked)).
//! The fullscreen route was not under that block: in fullscreen the
//! controls still answer, and the poll's pause still holds.
//!
//! Frames: each poll asks the player for the frame due now; a frame with a
//! new revision becomes a new image handle (its pixels shared, not copied),
//! so the renderer uploads a frame only when it changed.
//!
//! # Not as Flutter had it
//!
//! - iced clips to rectangles: the frame is not cut to the box's rounded
//!   corners (`ClipRRect`); the controls' gradient rounds its bottom
//!   corners to the border's inner radius.
//! - The poster starts the player on its press (Dart's `onTap`, on the
//!   release): the controls, made then, time their first double press
//!   from the press.
//! - iced has no focus scope. In Flutter a text field that took the focus
//!   kept it from the controls for good; here a focused field takes the
//!   keys it gets, and once none does the controls that hold the keyboard
//!   hear them again.
//! - A page that leaves while its video is fullscreen takes the
//!   fullscreen view with it, and the window stays natively fullscreen;
//!   media_kit's route on the root navigator would have outlived the page.

use std::sync::Arc;
use std::time::Duration;

use crate::ui::core::text;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::widget::image::{self, FilterMethod};
use iced::widget::{container, mouse_area, opaque, stack, Space};
use iced::{
    keyboard, mouse, window, Color, ContentFit, Element, Event, Length, Rectangle, Renderer, Size,
    Subscription, Task, Theme, Vector,
};
use vocara_services::media_player::{clamp_volume, MediaKind, MediaPlayerFactory, VideoFrame};

use super::technique_audio_player::{is_unavailable_path, Transport, UNAVAILABLE_CLIP_MESSAGE};
use super::video_controls::{self as controls, seek_target, video_controls, Command, Shortcut};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, boxed, Studio};
use crate::ui::core::widgets::holds_keyboard::holds_keyboard;
use crate::ui::core::widgets::wrap::Wrap;

/// How often a playing video is polled: a new frame can be due at 60 Hz.
pub const VIDEO_POLL_INTERVAL: Duration = Duration::from_micros(16_667);

/// The widest the player grows.
pub const MAX_WIDTH: f32 = 520.0;

/// The box's width : height.
pub const ASPECT_RATIO: f32 = 16.0 / 9.0;

/// The box's corner radius, and its border's width.
pub const RADIUS: f32 = 10.0;
pub const BORDER_WIDTH: f32 = 1.0;

/// The video player's messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The poster's play button: start the player.
    Play,
    /// The controls (inline or fullscreen).
    Controls(Command),
    /// A shortcut, for the controls that hold the keyboard.
    Key(Shortcut),
    /// The poll.
    Tick,
}

/// What the window must do when the video enters or leaves fullscreen
/// (media_kit's `defaultEnterNativeFullscreen` / `defaultExitNativeFullscreen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRequest {
    EnterFullscreen,
    ExitFullscreen,
}

impl WindowRequest {
    /// The window mode it asks for.
    pub fn mode(self) -> window::Mode {
        match self {
            WindowRequest::EnterFullscreen => window::Mode::Fullscreen,
            WindowRequest::ExitFullscreen => window::Mode::Windowed,
        }
    }
}

/// Carries `request` out on the app's window: its native fullscreen, or
/// back. A window already in that mode stays as it is (media_kit checked
/// the style mask first).
pub fn window_task<M: Send + 'static>(request: WindowRequest) -> Task<M> {
    let mode = request.mode();
    window::latest().and_then(move |id| window::set_mode(id, mode))
}

/// The frame on screen: its revision and its image.
#[derive(Debug, Clone)]
struct Shown {
    revision: u64,
    handle: image::Handle,
}

/// `TechniqueVideoPlayer`'s state (see the module docs).
#[derive(Debug)]
pub struct TechniqueVideoPlayer {
    path: String,
    /// No player until the first play.
    transport: Transport,
    /// True when the clip cannot be played here: its bytes are not on this
    /// device (the repository's `asset://` placeholder), or opening it
    /// failed.
    unavailable: bool,
    frame: Option<Shown>,
    /// media_kit's fullscreen route is up.
    fullscreen: bool,
}

impl TechniqueVideoPlayer {
    /// The poster for the clip at `path`; no player yet.
    pub fn new(path: &str) -> TechniqueVideoPlayer {
        TechniqueVideoPlayer {
            path: path.to_string(),
            transport: Transport::new(None),
            unavailable: is_unavailable_path(path),
            frame: None,
            fullscreen: false,
        }
    }

    /// Whether the clip shows as unavailable.
    pub fn is_unavailable(&self) -> bool {
        self.unavailable
    }

    /// Whether the player was started (Dart's `_controller != null`).
    pub fn has_player(&self) -> bool {
        self.transport.has_player()
    }

    /// Whether the player reports playing.
    pub fn is_playing(&self) -> bool {
        self.transport.playing()
    }

    /// Whether the video is in fullscreen.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// What the controls show.
    pub fn playback(&self) -> controls::Playback {
        self.transport.playback()
    }

    /// The revision of the frame on screen, if any.
    pub fn frame_revision(&self) -> Option<u64> {
        self.frame.as_ref().map(|f| f.revision)
    }

    /// `_start`: builds the player and opens the clip playing — once, and
    /// never for a clip that cannot play here. A failure drops (disposes)
    /// the player it built and shows the clip unavailable.
    fn start(&mut self, players: &dyn MediaPlayerFactory) {
        if self.transport.has_player() || self.unavailable {
            return;
        }
        let started = players.create(MediaKind::Video).and_then(|mut player| {
            player.open(&self.path, true)?;
            Ok(player)
        });
        match started {
            Ok(player) => {
                self.transport = Transport::new(Some(player));
                self.tick(false);
            }
            Err(_) => self.unavailable = true,
        }
    }

    /// Handles a message. `players` builds the player on the first play.
    /// Returns what the window must do, when the video enters or leaves
    /// fullscreen.
    pub fn update(
        &mut self,
        message: Message,
        players: &dyn MediaPlayerFactory,
        play_blocked: bool,
    ) -> Option<WindowRequest> {
        let request = match message {
            Message::Tick => {
                self.tick(play_blocked);
                return None;
            }
            // Neither the pointer nor the keyboard reach the inline video
            // while a take records: a press queued before the block lands
            // after it. The fullscreen route was not under the block.
            _ if play_blocked && !self.fullscreen => None,
            Message::Play => {
                self.start(players);
                None
            }
            Message::Controls(command) => self.command(command, play_blocked),
            Message::Key(shortcut) => self.shortcut(shortcut, play_blocked),
        };
        self.refresh_frame();
        request
    }

    /// What the controls asked.
    fn command(&mut self, command: Command, play_blocked: bool) -> Option<WindowRequest> {
        if !self.transport.has_player() {
            return None;
        }
        match command {
            Command::PlayOrPause => self.transport.play_or_pause(play_blocked),
            Command::Seek(to) => self.transport.seek(to),
            Command::SetVolume(volume) => self.transport.set_volume(clamp_volume(volume)),
            Command::ToggleFullscreen => return self.set_fullscreen(!self.fullscreen),
        }
        None
    }

    /// What a shortcut does (`keyboardShortcuts`, `material_desktop.dart:
    /// 526-575`), against the player's state now.
    fn shortcut(&mut self, shortcut: Shortcut, play_blocked: bool) -> Option<WindowRequest> {
        let state = self.transport.current()?;
        match shortcut {
            Shortcut::Play if !play_blocked => self.transport.play(),
            Shortcut::Play => {}
            Shortcut::Pause => self.transport.pause(),
            Shortcut::PlayOrPause => self.transport.play_or_pause(play_blocked),
            // A one-clip playlist: `next()` and `previous()` do nothing.
            Shortcut::Next | Shortcut::Previous => {}
            Shortcut::SeekBy(ms) => {
                if let Some(to) = seek_target(state.position, ms, state.duration) {
                    self.transport.seek(to);
                }
            }
            Shortcut::VolumeBy(step) => {
                self.transport.set_volume(clamp_volume(state.volume + step))
            }
            Shortcut::ToggleFullscreen => return self.set_fullscreen(!self.fullscreen),
            Shortcut::ExitFullscreen => return self.set_fullscreen(false),
        }
        None
    }

    /// `enterFullscreen` / `exitFullscreen`: nothing when already so.
    fn set_fullscreen(&mut self, on: bool) -> Option<WindowRequest> {
        if self.fullscreen == on {
            return None;
        }
        self.fullscreen = on;
        Some(if on {
            WindowRequest::EnterFullscreen
        } else {
            WindowRequest::ExitFullscreen
        })
    }

    /// `pauseSignal`: a take is arming; pause where it is.
    pub fn pause_signal(&mut self) {
        self.transport.pause();
    }

    /// Polls the player: its state, then the frame due now.
    fn tick(&mut self, play_blocked: bool) {
        let _ = self.transport.poll(play_blocked);
        self.refresh_frame();
    }

    /// Takes the player's latest frame if it is not the one on screen.
    fn refresh_frame(&mut self) {
        let Some(frame) = self.transport.frame() else {
            return;
        };
        if self.frame_revision() == Some(frame.revision) {
            return;
        }
        self.frame = Some(Shown {
            revision: frame.revision,
            handle: frame_handle(&frame),
        });
    }

    /// Whether the host should poll it now.
    pub fn needs_tick(&self) -> bool {
        self.transport.needs_tick()
    }

    /// The poll, while it needs one.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.needs_tick() {
            iced::time::every(VIDEO_POLL_INTERVAL).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// The latest frame, letterboxed (`BoxFit.contain`, low filter
    /// quality), over black.
    fn picture(&self) -> Element<'_, Message> {
        let picture: Element<'_, Message> = match &self.frame {
            Some(shown) => image::Image::new(shown.handle.clone())
                .content_fit(ContentFit::Contain)
                .filter_method(FilterMethod::Linear)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => Space::new().width(Length::Fill).height(Length::Fill).into(),
        };
        container(picture)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| boxed(Some(Color::BLACK), 0.0, None))
            .into()
    }

    pub fn view(&self, play_blocked: bool) -> Element<'_, Message> {
        let content: Element<'_, Message> = if !self.transport.has_player() {
            poster(self.unavailable, play_blocked)
        } else {
            stack![
                self.picture(),
                video_controls(self.playback(), Message::Controls)
                    .enabled(!play_blocked)
                    .corner_radius(RADIUS - BORDER_WIDTH),
            ]
            .into()
        };
        container(AspectRatio::new(content, ASPECT_RATIO))
            .width(Length::Fill)
            .max_width(MAX_WIDTH)
            .padding(BORDER_WIDTH)
            .clip(true)
            .style(|_| boxed(Some(Color::BLACK), RADIUS, Some(Studio::BORDER)))
            .into()
    }

    /// The fullscreen route's page while the video is fullscreen: the video
    /// filling the window over black, its own controls over it; it takes
    /// every click and every key (its shortcuts first).
    pub fn fullscreen_view(&self) -> Option<Element<'_, Message>> {
        if !self.fullscreen || !self.transport.has_player() {
            return None;
        }
        let page = stack![
            self.picture(),
            video_controls(self.playback(), Message::Controls).fullscreen(true),
        ];
        let keys = Wrap::<Message, ()>::new(
            container(page).width(Length::Fill).height(Length::Fill),
            Box::new(|_, event, _, _, shell| {
                if shell.is_event_captured() {
                    return;
                }
                if let Event::Keyboard(key) = event {
                    if let Some(shortcut) = controls::shortcut(key) {
                        shell.publish(Message::Key(shortcut));
                        shell.capture_event();
                    }
                }
            }),
        );
        Some(holds_keyboard(opaque(keys)))
    }
}

/// The shortcut message a key event triggers, if any (for the host that
/// routes the keyboard to the inline controls).
pub fn key_message(event: &keyboard::Event) -> Option<Message> {
    controls::shortcut(event).map(Message::Key)
}

/// A frame as an image handle sharing its pixels.
fn frame_handle(frame: &VideoFrame) -> image::Handle {
    struct Pixels(Arc<Vec<u8>>);
    impl AsRef<[u8]> for Pixels {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    image::Handle::from_rgba(
        frame.width,
        frame.height,
        bytes::Bytes::from_owner(Pixels(frame.rgba.clone())),
    )
}

/// `_Poster`: a dark surface with a centered play button, or the
/// unavailable notice when the clip cannot be played here.
fn poster<'a>(unavailable: bool, play_blocked: bool) -> Element<'a, Message> {
    let black = |_: &Theme| boxed(Some(Color::BLACK), 0.0, None);
    if unavailable {
        return container(
            text(UNAVAILABLE_CLIP_MESSAGE)
                .size(12)
                .color(Studio::TEXT_FAINT),
        )
        .center(Length::Fill)
        .style(black)
        .into();
    }
    let button = container(icons::icon(Icon::Play, 24.0, Color::WHITE))
        .center_x(54)
        .center_y(54)
        .style(|_| boxed(Some(alpha(Studio::ACCENT, 0.9)), 27.0, None));
    let surface = container(button).center(Length::Fill).style(black);
    if play_blocked {
        return surface.into();
    }
    mouse_area(surface)
        .on_press(Message::Play)
        .interaction(mouse::Interaction::Pointer)
        .into()
}

/// Flutter's `AspectRatio`: as wide as it may be, and `ratio` times
/// narrower than that tall (shrunk to fit a height limit); its child fills
/// it.
struct AspectRatio<'a, M> {
    content: Element<'a, M>,
    ratio: f32,
}

impl<'a, M> AspectRatio<'a, M> {
    fn new(content: impl Into<Element<'a, M>>, ratio: f32) -> Self {
        AspectRatio {
            content: content.into(),
            ratio,
        }
    }
}

/// The size `AspectRatio` takes within `max`.
fn aspect_size(max: Size, ratio: f32) -> Size {
    let mut width = max.width;
    if !width.is_finite() {
        width = if max.height.is_finite() {
            max.height * ratio
        } else {
            0.0
        };
    }
    let mut height = width / ratio;
    if height > max.height {
        height = max.height;
        width = height * ratio;
    }
    Size::new(width, height)
}

impl<M> Widget<M, Theme, Renderer> for AspectRatio<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = aspect_size(limits.max(), self.ratio);
        let child = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(size, size),
        );
        layout::Node::with_children(size, vec![child])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
        }
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                child,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        layout
            .children()
            .next()
            .map_or(mouse::Interaction::default(), |child| {
                self.content.as_widget().mouse_interaction(
                    &tree.children[0],
                    child,
                    cursor,
                    viewport,
                    renderer,
                )
            })
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let child = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<AspectRatio<'a, M>> for Element<'a, M> {
    fn from(widget: AspectRatio<'a, M>) -> Self {
        Element::new(widget)
    }
}

#[cfg(test)]
mod tests;
