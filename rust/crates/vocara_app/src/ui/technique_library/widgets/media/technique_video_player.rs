//! Port of `ui/technique_library/widgets/media/technique_video_player.dart`:
//! an inline video player for a technique example clip.
//!
//! To avoid spinning up a native player (and a texture) for every clip just
//! by showing the viewer, the player is created LAZILY on the first press of
//! the poster's play button; a lightweight poster shows until then. Once
//! started, the latest frame shows letterboxed in the 16:9 box, and the
//! controls appear while the pointer is over it.
//!
//! The controls: media_kit's desktop controls (play/pause, seek bar,
//! position / duration, volume, fullscreen, keyboard shortcuts) become the
//! audio player's bar — its disc, scrubber and readout — over the bottom of
//! the picture. Volume, fullscreen and the shortcuts are not ported; the
//! controls show while hovered (media_kit also hid them after a few idle
//! seconds).
//!
//! While a take records (`play_blocked`), the poster and the controls take
//! no input (Dart's `IgnorePointer` + `ExcludeFocus`), and anything that
//! starts the player anyway is paused again on the next poll
//! ([`pause_while_play_blocked`](super::technique_audio_player::pause_while_play_blocked)).
//!
//! Frames: each poll asks the player for the frame due now; a frame with a
//! new revision becomes a new image handle (its pixels shared, not copied),
//! so the renderer uploads a frame only when it changed.

use std::sync::Arc;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::image::{self, FilterMethod};
use iced::widget::{column, container, mouse_area, stack, text, Space};
use iced::{
    mouse, Color, ContentFit, Element, Event, Length, Rectangle, Renderer, Size, Subscription,
    Theme, Vector,
};
use vocara_services::media_player::{MediaKind, MediaPlayerFactory, VideoFrame};

use super::technique_audio_player::{
    self as audio, is_unavailable_path, Transport, UNAVAILABLE_CLIP_MESSAGE,
};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, boxed, Studio};

/// How often a playing video is polled: a new frame can be due at 60 Hz.
pub const VIDEO_POLL_INTERVAL: Duration = Duration::from_micros(16_667);

/// The widest the player grows.
pub const MAX_WIDTH: f32 = 520.0;

/// The box's width : height.
pub const ASPECT_RATIO: f32 = 16.0 / 9.0;

/// The video player's messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The poster's play button: start the player.
    Play,
    /// The pointer entered (`true`) or left the picture: the controls.
    Hover(bool),
    /// The controls (the audio player's bar).
    Controls(audio::Message),
    /// The poll.
    Tick,
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
    hovered: bool,
    frame: Option<Shown>,
}

impl TechniqueVideoPlayer {
    /// The poster for the clip at `path`; no player yet.
    pub fn new(path: &str) -> TechniqueVideoPlayer {
        TechniqueVideoPlayer {
            path: path.to_string(),
            transport: Transport::new(None),
            unavailable: is_unavailable_path(path),
            hovered: false,
            frame: None,
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
    pub fn update(
        &mut self,
        message: Message,
        players: &dyn MediaPlayerFactory,
        play_blocked: bool,
    ) {
        match message {
            Message::Tick => self.tick(play_blocked),
            Message::Hover(hovered) => self.hovered = hovered,
            // Neither the pointer nor the keyboard while a take records: a
            // press queued before the block lands after it.
            _ if play_blocked => {}
            Message::Play => self.start(players),
            Message::Controls(m) => {
                let _ = self.transport.update(m, play_blocked);
                self.refresh_frame();
            }
        }
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

    pub fn view(&self, play_blocked: bool) -> Element<'_, Message> {
        let content: Element<'_, Message> = if !self.transport.has_player() {
            poster(self.unavailable, play_blocked)
        } else {
            let picture: Element<'_, Message> = match &self.frame {
                Some(shown) => image::Image::new(shown.handle.clone())
                    .content_fit(ContentFit::Contain)
                    .filter_method(FilterMethod::Linear)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                None => Space::new().width(Length::Fill).height(Length::Fill).into(),
            };
            let mut layers = stack![container(picture)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| boxed(Some(Color::BLACK), 0.0, None))];
            if self.hovered && !play_blocked {
                layers = layers.push(column![
                    Space::new().height(Length::Fill),
                    container(self.transport.bar(play_blocked).map(Message::Controls))
                        .width(Length::Fill)
                        .height(audio::HEIGHT)
                        .padding([0, 10])
                        .align_y(Vertical::Center)
                        .style(|_| boxed(Some(alpha(Color::BLACK, 0.55)), 0.0, None)),
                ]);
            }
            let area = mouse_area(layers);
            if play_blocked {
                area.into()
            } else {
                area.on_enter(Message::Hover(true))
                    .on_exit(Message::Hover(false))
                    .into()
            }
        };
        container(AspectRatio::new(content, ASPECT_RATIO))
            .width(Length::Fill)
            .max_width(MAX_WIDTH)
            .clip(true)
            .style(|_| boxed(Some(Color::BLACK), 10.0, Some(Studio::BORDER)))
            .into()
    }
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
