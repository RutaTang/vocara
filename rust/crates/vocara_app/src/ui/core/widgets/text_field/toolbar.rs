//! The context menu a right-click opened in a macOS Flutter text field:
//! `TextField`'s and `SelectableText`'s default `contextMenuBuilder`,
//! `AdaptiveTextSelectionToolbar.editableText`, which on macOS is a
//! `CupertinoDesktopTextSelectionToolbar` of
//! `CupertinoDesktopTextSelectionToolbarButton.text` items (Flutter
//! 3.38's `cupertino/desktop_text_selection_toolbar*.dart`), drawn by
//! Flutter in an overlay, not a native menu.
//!
//! **What it offers** (`EditableText.contextMenuButtonItems` with
//! `cupertinoDesktopTextSelectionHandleControls`): Cut when the field is
//! editable and the selection is not collapsed, Copy when the selection is
//! not collapsed, Paste when the field is editable and the clipboard holds
//! text, in that order, and never Select All (`selectAllEnabled` is false
//! on macOS). The items follow the selection while the menu is up (its
//! builder ran again as the selection changed); with none, nothing shows,
//! though the menu is still up (Escape still only closes it).
//!
//! **What they do** (`copySelection`, `cutSelection`, `pasteText` with
//! `SelectionChangedCause.toolbar`): Copy puts the selection on the
//! clipboard and keeps it; Cut puts it there and deletes it; Paste puts the
//! clipboard's text in place of the selection, the caret after it. Each
//! closes the menu. An item acts when the primary button comes up on it,
//! as `CupertinoButton`'s tap did; while held it fades to 0.7 opacity over
//! 120 ms (`Curves.easeInOutCubicEmphasized`), back over 180 ms
//! (`Curves.easeOutCubic`) when let go or left (`tapMoveSlop` is 0 on
//! macOS), through `Curves.decelerate`, and a run is never cut short: the
//! next starts when it ends (`_CupertinoButtonState._animate`).
//!
//! **What closes it**: a press anywhere outside it (in its field the press
//! goes on to the field, as `onTapDown` hid the toolbar on macOS; outside,
//! `TextFieldTapRegion` took the field's focus away), Escape (the field's
//! `DismissIntent`, which then goes no further), a change to the text, the
//! field losing focus, the window losing focus, and one of its items.
//! Scrolling leaves it where it is (`_platformSupportsFadeOnScroll` is
//! false on macOS, and its anchor is the click's place in the window).
//!
//! **Where it goes** (`DesktopTextSelectionToolbarLayoutDelegate`): its top
//! left at the right-click, inside the window less 8 on every side
//! (`_kToolbarScreenPadding`), moved left and up by what would overhang the
//! right and bottom.
//!
//! **How it looks**, dark (the zinc theme's brightness): 222 wide, radius
//! 8, 6 padding, `0xB2303030` with a 1 px `0xFF5B5B5B` border, under a
//! shadow of `0x3C000000` blurred 10 at (0, 4), spread 0.5. Each item is a
//! label 14 px, w400, letter spacing -0.15, white, 8 in from the sides, 2
//! below the top and 5 above the bottom; under the pointer it fills with
//! the theme's primary color (`CupertinoTheme.primaryColor`, zinc's primary
//! under a Material theme), radius 4, and its label takes
//! `primaryContrastingColor` (zinc's primary foreground). The labels are
//! Cupertino's English ones (`DefaultCupertinoLocalizations`).
//!
//! Its corners and the items' are rounded superellipses
//! (`RoundedSuperellipseBorder`, Apple's continuous corners, drawn from the
//! engine's own construction: [`rsuperellipse`](super::super::rsuperellipse)),
//! filled and stroked as canvas paths: the fill over the whole box, the
//! 1 px border centred half a pixel in on a radius of 7.5 (the side's
//! inside stroke, `RSuperellipse.inflate(-0.5)`). The shadow's shape is the
//! box grown by its 0.5 spread on every side (`ShapeDecoration` shadows
//! `rect.shift(offset).inflate(spreadRadius)`).
//!
//! Behind the box, the toolbar blurred what lay there (its own shadow
//! included) by a Gaussian of sigma 20, then saturated it threefold
//! (`BackdropFilter` with `ImageFilter.compose(outer:
//! ColorFilter.matrix(_matrixWithSaturation(3)), inner:
//! ImageFilter.blur(20, 20))`, whose blur, composed, takes the window's
//! edge pixels beyond it), inside the box's hard-edged clip: the renderer
//! does the same (`opacity_layer::Effects::draw_backdrop_filter`), and the
//! box and its items go over it in a later layer. Each item fades as one
//! layer (its hover fill and label together, the `FadeTransition` inside
//! `CupertinoButton`).
//!
//! What iced cannot draw as Flutter did, kept as close as it can be:
//! - its shadow was its shape blurred by a Gaussian (`MaskFilter.blur`,
//!   sigma 6.27 for the blur radius 10); iced's shadow is a quad's (a
//!   circular-cornered box, radius 8, faded out over the blur radius on
//!   its own curve), so only its place and spread are Flutter's;
//! - the hard-edged clip of the box's shape (`Clip.hardEdge`) also cut its
//!   fill and border; iced has no clip of a shape, so those are filled
//!   antialiased (the filtered backdrop alone is cut hard);
//! - the labels' style set `inherit: false` and no family, so Flutter set
//!   them in the macOS system UI font (SF Pro, CoreText's `.AppleSystemUIFont`),
//!   a variable face whose optical size CoreText set from the text's size,
//!   where cosmic-text (iced's text engine) varies a face along its weight
//!   axis alone and draws every other axis at its default. The labels take
//!   that face, at its own line box, only where its default is the 14 px
//!   instance and the shapers' tracking agrees (read off the face, see
//!   [`toolbar_face`](super::toolbar_face)); otherwise, and off macOS, they
//!   are in the bundled Inter, at Inter's natural line height (1.21 em),
//!   the box Flutter's line took from its font's metrics.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::graphics::text::cosmic_text::Buffer;
use iced::advanced::graphics::text::{font_system, Version};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::text::{Alignment, Wrapping};
use iced::advanced::{clipboard, overlay, Clipboard, Shell};
use iced::time::Instant;
use iced::widget::canvas::{Frame, Stroke};
use iced::{
    keyboard, mouse, window, Border, Color, Event, Padding, Point, Rectangle, Renderer, Shadow,
    Size, Theme, Vector,
};

use crate::ui::core::motion::{cubic_curve, Lerp};
use crate::ui::core::theme::Zinc;
use crate::ui::core::widgets::opacity_layer::{BackdropFilter, Effects as _};
use crate::ui::core::widgets::rsuperellipse;
use crate::ui::core::widgets::studio_text::{draw_line, lay_out, Spec};

use super::toolbar_face::label_face;

/// `_kToolbarScreenPadding`: the room kept between the toolbar and the
/// window's edges.
pub const SCREEN_PADDING: f32 = 8.0;
/// `_kToolbarWidth`.
pub const WIDTH: f32 = 222.0;
/// `_kToolbarBorderRadius`.
pub const RADIUS: f32 = 8.0;
/// `_kToolbarPadding`.
pub const PADDING: f32 = 6.0;
/// `_kToolbarBlurSigma`: the backdrop's blur.
pub const BLUR_SIGMA: f32 = 20.0;
/// `_kToolbarSaturationBoost`: the blurred backdrop's saturation, scaled.
pub const SATURATION: f32 = 3.0;
/// `_kToolbarBackgroundColor`, dark.
pub const BACKGROUND: Color = Color::from_rgba8(0x30, 0x30, 0x30, 0xB2 as f32 / 255.0);
/// `_kToolbarBorderColor`, dark.
pub const BORDER: Color = Color::from_rgb8(0x5B, 0x5B, 0x5B);
/// `_kToolbarShadow`, but for its spread.
pub const SHADOW: Shadow = Shadow {
    color: Color::from_rgba8(0, 0, 0, 60.0 / 255.0),
    offset: Vector::new(0.0, 4.0),
    blur_radius: 10.0,
};
/// `_kToolbarShadow`'s `spreadRadius`: how far its shape grows past the
/// toolbar's on every side.
pub const SHADOW_SPREAD: f32 = 0.5;
/// The toolbar's border's width (`BorderSide`'s default).
pub const BORDER_WIDTH: f32 = 1.0;
/// `_kToolbarButtonPadding`.
pub const BUTTON_PADDING: Padding = Padding {
    top: 2.0,
    right: 8.0,
    bottom: 5.0,
    left: 8.0,
};
/// The items' `CupertinoButton.borderRadius`.
pub const BUTTON_RADIUS: f32 = 4.0;
/// `_kToolbarButtonFontStyle`'s size and letter spacing.
pub const LABEL_SIZE: f32 = 14.0;
pub const LABEL_SPACING: f32 = -0.15;
/// Inter's natural line box (ascent + descent), in ems: the labels' unless
/// they are in the system UI font ([`label_face`]).
pub const LABEL_LINE_HEIGHT: f32 = 1.21;
/// An item's label: `CupertinoColors.white` (dark).
pub const LABEL: Color = Color::WHITE;
/// An item under the pointer: its fill (`primaryColor`) and its label
/// (`primaryContrastingColor`).
pub const HOVER: Color = Zinc::PRIMARY;
pub const HOVER_LABEL: Color = Zinc::PRIMARY_FOREGROUND;
/// `CupertinoButton.pressedOpacity` as the toolbar set it.
pub const PRESSED_OPACITY: f32 = 0.7;
/// `_CupertinoButtonState.kFadeOutDuration` / `kFadeInDuration`.
pub const FADE_OUT: Duration = Duration::from_millis(120);
pub const FADE_IN: Duration = Duration::from_millis(180);
/// How far a pointer may move between a secondary press and its release
/// for the two to be a tap (`kTouchSlop`, the tap recognizer's default).
pub const TAP_SLOP: f32 = 18.0;

/// An item of the menu, in the order it lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Cut,
    Copy,
    Paste,
}

impl Item {
    /// `DefaultCupertinoLocalizations`' label.
    pub fn label(self) -> &'static str {
        match self {
            Item::Cut => "Cut",
            Item::Copy => "Copy",
            Item::Paste => "Paste",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The items a field offers: `editable` or read-only, with a selection or
/// a collapsed one, the clipboard holding text or not.
pub fn items(editable: bool, has_selection: bool, can_paste: bool) -> Vec<Item> {
    let mut items = Vec::with_capacity(3);
    if editable && has_selection {
        items.push(Item::Cut);
    }
    if has_selection {
        items.push(Item::Copy);
    }
    if editable && can_paste {
        items.push(Item::Paste);
    }
    items
}

/// Whether the clipboard holds text (`Clipboard.hasStrings`).
pub fn clipboard_has_text(clipboard: &mut dyn Clipboard) -> bool {
    clipboard
        .read(clipboard::Kind::Standard)
        .is_some_and(|text| !text.is_empty())
}

/// An item's height: its padding around one label line.
pub fn item_height() -> f32 {
    BUTTON_PADDING.top + LABEL_SIZE * label_face().line_height + BUTTON_PADDING.bottom
}

/// The toolbar's size with `count` items.
pub fn size(count: usize) -> Size {
    Size::new(WIDTH, 2.0 * PADDING + count as f32 * item_height())
}

/// Where a toolbar of `size` goes for a right-click at `anchor` in a
/// window of `window` (`DesktopTextSelectionToolbarLayoutDelegate`, inside
/// the screen padding).
pub fn place(anchor: Point, size: Size, window: Size) -> Point {
    let room = Size::new(
        window.width - 2.0 * SCREEN_PADDING,
        window.height - 2.0 * SCREEN_PADDING,
    );
    let local = anchor - Vector::new(SCREEN_PADDING, SCREEN_PADDING);
    let fit = |at: f32, extent: f32, room: f32| {
        let overhang = at + extent - room;
        if overhang > 0.0 {
            at - overhang
        } else {
            at
        }
    };
    Point::new(
        SCREEN_PADDING + fit(local.x, size.width, room.width),
        SCREEN_PADDING + fit(local.y, size.height, room.height),
    )
}

/// An open menu, kept in its field's state.
#[derive(Debug, Clone)]
pub struct State {
    /// The right-click, in the field's layout coordinates.
    anchor: Point,
    /// The right-click in the window, fixed when the menu first shows.
    fixed: Option<Point>,
    /// Whether the clipboard held text when the menu opened.
    can_paste: bool,
    /// The item under the pointer.
    hovered: Option<Item>,
    /// The item the primary button went down on, while it is down.
    pressed: Option<Item>,
    /// Each item's press fade.
    fades: [Fade; 3],
    /// The last frame's time.
    now: Option<Instant>,
}

impl State {
    /// A menu for a right-click at `anchor` (the field's coordinates).
    pub fn new(anchor: Point, can_paste: bool) -> Self {
        Self {
            anchor,
            fixed: None,
            can_paste,
            hovered: None,
            pressed: None,
            fades: [Fade::default(); 3],
            now: None,
        }
    }

    /// Whether the clipboard held text when it opened.
    pub fn can_paste(&self) -> bool {
        self.can_paste
    }

    /// Where it is anchored in the window, given the field's `translation`
    /// to the window now: the first time it is asked, the right-click's
    /// place then, which it keeps.
    pub fn position(&mut self, translation: Vector) -> Point {
        *self.fixed.get_or_insert(self.anchor + translation)
    }
}

/// A field that has a menu: what it offers and what its items do.
pub trait Field<Message> {
    /// Its open menu.
    fn menu(&self) -> Option<&State>;
    /// Its open menu, to change.
    fn menu_mut(&mut self) -> Option<&mut State>;
    /// The items it offers now.
    fn items(&self) -> Vec<Item>;
    /// Carries `item` out, closing the menu.
    fn perform(
        &mut self,
        item: Item,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    );
    /// Closes the menu.
    fn close(&mut self);
}

/// The open menu of `field` as an overlay over everything, anchored at
/// `position` in the window.
pub fn overlay<'b, Message: 'b>(
    field: Box<dyn Field<Message> + 'b>,
    position: Point,
) -> overlay::Element<'b, Message, Theme, Renderer> {
    overlay::Element::new(Box::new(Toolbar { field, position }))
}

struct Toolbar<'b, Message> {
    field: Box<dyn Field<Message> + 'b>,
    position: Point,
}

/// An item's label laid out (once per label and thread), and its size.
fn label(item: Item) -> (Arc<Buffer>, Size) {
    /// A label laid out, with the font system's version it was laid out
    /// at (a font loaded since lays it out afresh).
    type Laid = (Item, Version, Arc<Buffer>, Size);
    thread_local! {
        static LABELS: RefCell<Vec<Laid>> = const { RefCell::new(Vec::new()) };
    }
    // Before the font system's lock: the first call takes it.
    let face = label_face();
    let version = font_system().read().expect("Read font system").version();
    LABELS.with(|labels| {
        let found = labels
            .borrow()
            .iter()
            .find(|(i, v, ..)| *i == item && *v == version)
            .map(|(_, _, buffer, size)| (buffer.clone(), *size));
        if let Some(found) = found {
            return found;
        }
        let spec = Spec {
            content: item.label(),
            size: LABEL_SIZE,
            line_height: LABEL_SIZE * face.line_height,
            font: face.font,
            align_x: Alignment::Default,
            wrapping: Wrapping::None,
            letter_spacing: LABEL_SPACING,
            tabular: false,
            max_lines: Some(1),
        };
        let laid = {
            let mut font_system = font_system().write().expect("Write font system");
            lay_out(font_system.raw(), &spec, Size::INFINITE)
        };
        let buffer = Arc::new(laid.buffer);
        let mut labels = labels.borrow_mut();
        labels.retain(|(i, ..)| *i != item);
        labels.push((item, version, buffer.clone(), laid.min_bounds));
        (buffer, laid.min_bounds)
    })
}

/// The item under `cursor` in a toolbar laid out at `bounds` with `items`.
fn item_at(bounds: Rectangle, items: &[Item], cursor: mouse::Cursor) -> Option<Item> {
    let position = cursor.position_over(bounds)?;
    let x = position.x - bounds.x;
    let y = position.y - bounds.y - PADDING;
    if x < PADDING || x >= bounds.width - PADDING || y < 0.0 {
        return None;
    }
    items.get((y / item_height()) as usize).copied()
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for Toolbar<'_, Message> {
    fn layout(&mut self, _renderer: &Renderer, bounds: Size) -> layout::Node {
        let count = self.field.items().len();
        let size = if count == 0 { Size::ZERO } else { size(count) };
        layout::Node::new(size).move_to(place(self.position, size, bounds))
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let items = self.field.items();
        let Some(menu) = self.field.menu() else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let bounds = layout.bounds();
        // The shadow alone, its shape the box grown by its spread.
        renderer.fill_quad(
            renderer::Quad {
                bounds: bounds.expand(SHADOW_SPREAD),
                border: Border {
                    radius: RADIUS.into(),
                    ..Border::default()
                },
                shadow: SHADOW,
                snap: false,
            },
            Color::TRANSPARENT,
        );
        // What lies behind the box, the shadow included, blurred and
        // saturated inside its hard-edged clip.
        renderer.draw_backdrop_filter(
            &rsuperellipse::path(bounds, RADIUS),
            BackdropFilter {
                blur_sigma: BLUR_SIGMA,
                color_matrix: BackdropFilter::saturation_matrix(SATURATION),
            },
        );
        // The rest over it, in a later layer.
        renderer.with_layer(bounds.expand(1.0), |renderer| {
            draw_box(renderer, bounds, &items, menu, cursor);
        });
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let items = self.field.items();
        let bounds = layout.bounds();
        let over = !items.is_empty() && cursor.is_over(bounds);
        let at = item_at(bounds, &items, cursor);
        let Some(menu) = self.field.menu_mut() else {
            return;
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                if !over {
                    // A press anywhere else closes it, and goes on.
                    self.field.close();
                    shell.request_redraw();
                    return;
                }
                if let (mouse::Button::Left, Some(item)) = (button, at) {
                    menu.pressed = Some(item);
                    menu.fades[item.index()].hold(true);
                    shell.request_redraw();
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let Some(item) = menu.pressed.take() else {
                    return;
                };
                menu.fades[item.index()].hold(false);
                shell.capture_event();
                shell.request_redraw();
                if at == Some(item) {
                    self.field.perform(item, clipboard, shell);
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if menu.hovered != at {
                    menu.hovered = at;
                    shell.request_redraw();
                }
                if let Some(item) = menu.pressed {
                    let fade = &mut menu.fades[item.index()];
                    let held = at == Some(item);
                    if fade.held != held {
                        fade.hold(held);
                        shell.request_redraw();
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                self.field.close();
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                menu.now = Some(*now);
                let mut moving = false;
                for fade in &mut menu.fades {
                    fade.advance(*now);
                    moving |= fade.is_animating(*now);
                }
                if moving {
                    shell.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.field.items().is_empty() && cursor.is_over(layout.bounds()) {
            // `CupertinoButton`'s cursor defers off the web: the arrow.
            mouse::Interaction::Idle
        } else {
            mouse::Interaction::None
        }
    }
}

/// The box (its fill and border, superellipses) and its items over the
/// filtered backdrop: each item (the hovered one's fill and its label) as
/// one layer at its press fade's opacity (the item's `FadeTransition`).
fn draw_box(
    renderer: &mut Renderer,
    bounds: Rectangle,
    items: &[Item],
    menu: &State,
    cursor: mouse::Cursor,
) {
    let hovered = item_at(bounds, items, cursor);
    let now = menu.now.unwrap_or_else(Instant::now);
    let mut frame = Frame::new(renderer, bounds.size());
    let local = Rectangle::with_size(bounds.size());
    frame.fill(&rsuperellipse::path(local, RADIUS), BACKGROUND);
    frame.stroke(
        &rsuperellipse::path(
            local.shrink(BORDER_WIDTH / 2.0),
            RADIUS - BORDER_WIDTH / 2.0,
        ),
        Stroke::default()
            .with_width(BORDER_WIDTH)
            .with_color(BORDER),
    );
    renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
        renderer.draw_geometry(frame.into_geometry());
    });
    for (i, item) in items.iter().enumerate() {
        let row = Rectangle {
            x: bounds.x + PADDING,
            y: bounds.y + PADDING + i as f32 * item_height(),
            width: bounds.width - 2.0 * PADDING,
            height: item_height(),
        };
        let is_hovered = hovered == Some(*item);
        let draw = |renderer: &mut Renderer| {
            if is_hovered {
                let mut frame = Frame::new(renderer, row.size());
                frame.fill(
                    &rsuperellipse::path(Rectangle::with_size(row.size()), BUTTON_RADIUS),
                    HOVER,
                );
                renderer.with_translation(Vector::new(row.x, row.y), |renderer| {
                    renderer.draw_geometry(frame.into_geometry());
                });
            }
            let (buffer, _) = label(*item);
            draw_line(
                renderer,
                &buffer,
                Point::new(row.x + BUTTON_PADDING.left, row.y + BUTTON_PADDING.top),
                if is_hovered { HOVER_LABEL } else { LABEL },
                Rectangle {
                    x: row.x + BUTTON_PADDING.left,
                    width: row.width - BUTTON_PADDING.x(),
                    ..row
                },
            );
        };
        let opacity = menu.fades[item.index()].opacity(now);
        if opacity < 1.0 {
            renderer.with_opacity_layer(row.expand(1.0), opacity, draw);
        } else {
            draw(renderer);
        }
    }
}

/// One item's press fade: `CupertinoButton`'s `_animationController`
/// (0 at rest, 1 held down).
#[derive(Debug, Clone, Copy, Default)]
pub struct Fade {
    /// `_buttonHeldDown`.
    held: bool,
    /// The value at rest, or where the run under way started.
    from: f32,
    /// The run under way.
    run: Option<Run>,
}

#[derive(Debug, Clone, Copy)]
struct Run {
    to: f32,
    /// Its first frame (`None` until the frame after it was asked for, as
    /// a ticker's elapsed time starts at its first tick).
    start: Option<Instant>,
    duration: Duration,
}

impl Fade {
    /// The controller's value at `now`.
    fn value(&self, now: Instant) -> f32 {
        let Some(Run {
            to,
            start: Some(start),
            duration,
        }) = self.run
        else {
            return self.from;
        };
        let t = (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32())
            .clamp(0.0, 1.0);
        let curved = if to > self.from {
            ease_in_out_cubic_emphasized(t)
        } else {
            // `Curves.easeOutCubic`.
            cubic_curve(0.215, 0.61, 0.355, 1.0, t)
        };
        self.from.lerp(to, curved)
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.run
            .is_some_and(|run| run.start.is_none_or(|start| now < start + run.duration))
    }

    /// The item's opacity at `now`: `Tween(1, pressedOpacity)` through
    /// `Curves.decelerate`.
    pub fn opacity(&self, now: Instant) -> f32 {
        let t = self.value(now);
        let decelerated = 1.0 - (1.0 - t) * (1.0 - t);
        1.0.lerp(PRESSED_OPACITY, decelerated)
    }

    /// The button went down or up, or the pointer left it or came back.
    pub(super) fn hold(&mut self, held: bool) {
        self.held = held;
        self.animate();
    }

    /// `_animate`: a run under way is left to finish.
    fn animate(&mut self) {
        if self.run.is_some() {
            return;
        }
        let (to, duration) = if self.held {
            (1.0, FADE_OUT)
        } else {
            (0.0, FADE_IN)
        };
        if self.from != to {
            self.run = Some(Run {
                to,
                start: None,
                duration,
            });
        }
    }

    /// At a frame: a run asked for starts, one that has ended settles, and
    /// if the button was let go or pressed meanwhile the next run is asked
    /// for (`ticker.then`).
    pub(super) fn advance(&mut self, now: Instant) {
        let Some(run) = &mut self.run else {
            return;
        };
        let Some(start) = run.start else {
            run.start = Some(now);
            return;
        };
        if now >= start + run.duration {
            self.from = run.to;
            self.run = None;
            self.animate();
        }
    }
}

/// `Curves.easeInOutCubicEmphasized`: `ThreePointCubic((0.05, 0),
/// (0.133333, 0.06), (0.166666, 0.4), (0.208333, 0.82), (0.25, 1))`.
fn ease_in_out_cubic_emphasized(t: f32) -> f32 {
    const A1: (f64, f64) = (0.05, 0.0);
    const B1: (f64, f64) = (0.133333, 0.06);
    const MID: (f64, f64) = (0.166666, 0.4);
    const A2: (f64, f64) = (0.208333, 0.82);
    const B2: (f64, f64) = (0.25, 1.0);
    if t <= 0.0 || t >= 1.0 {
        return t.clamp(0.0, 1.0);
    }
    let t = f64::from(t);
    let first = t < MID.0;
    let scale_x = if first { MID.0 } else { 1.0 - MID.0 };
    let scale_y = if first { MID.1 } else { 1.0 - MID.1 };
    let scaled = ((t - if first { 0.0 } else { MID.0 }) / scale_x) as f32;
    let y = if first {
        f64::from(cubic_curve(
            A1.0 / scale_x,
            A1.1 / scale_y,
            B1.0 / scale_x,
            B1.1 / scale_y,
            scaled,
        )) * scale_y
    } else {
        f64::from(cubic_curve(
            (A2.0 - MID.0) / scale_x,
            (A2.1 - MID.1) / scale_y,
            (B2.0 - MID.0) / scale_x,
            (B2.1 - MID.1) / scale_y,
            scaled,
        )) * scale_y
            + MID.1
    };
    y as f32
}
