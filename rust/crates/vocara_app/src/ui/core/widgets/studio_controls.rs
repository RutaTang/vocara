//! Port of `ui/core/widgets/studio_controls.dart`: the mixer's controls —
//! the rotary pan knob, the horizontal volume fader, the fixed-width value
//! readouts, the on/off toggle and the round and mini icon buttons.
//!
//! Pointer devices: Dart limited the drags to every pointer kind but the
//! trackpad (`kPointerDragDevices`), because a two-finger trackpad swipe — how
//! the timeline pans — reached drag recognizers as a pan-zoom gesture and
//! turned (and saved) whatever control it crossed. iced delivers that swipe
//! as `WheelScrolled`, never as a button drag, and these controls take no
//! wheel input, so the swipe leaves them alone by construction.
//!
//! Unmounting mid-drag: Dart committed a drag in flight when its control was
//! disposed (a lazy list scrolled it away), since a disposed recognizer fires
//! no end. iced has no disposal callback, and its lists are not lazy: a
//! control stays in the tree while its list scrolls, and the release that
//! ends the drag reaches it wherever the pointer is. A control the screen
//! stops showing mid-drag owes its commit to the screen.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::graphics::text::cosmic_text::Buffer;
use iced::advanced::layout::{self, Layout};
use iced::advanced::text::LineHeight;
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{renderer, Widget};
use iced::alignment::Horizontal;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::{button, canvas as canvas_widget, column, container};
use iced::{
    mouse, Background, Border, Color, Element, Event, Font, Length, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use super::animated::{animated_container, Decoration};
use super::gesture::{Gesture, PanDoubleTap};
use super::studio_text::{self, studio_text};
use super::wrap::Wrap;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, font, Studio};
use crate::util::to_string_as_fixed;

// ------------------------------------------------------------------ pan knob

/// How many logical pixels of horizontal drag sweep the pan from center to
/// one side (`d.delta.dx / 80`).
pub const PAN_DRAG_PIXELS: f64 = 80.0;

/// The knob's `L##` / `C` / `R##` label for a pan in [-1, 1].
pub fn pan_label(value: f64) -> String {
    let p = (value * 100.0).round() as i64;
    if p == 0 {
        return "C".to_string();
    }
    if p < 0 {
        format!("L{}", -p)
    } else {
        format!("R{p}")
    }
}

/// The pan a drag reaches from `from` after a horizontal move of `dx`.
pub fn pan_after_drag(from: f64, dx: f64) -> f64 {
    (from + dx / PAN_DRAG_PIXELS).clamp(-1.0, 1.0)
}

/// `StudioPanKnob`: a compact rotary pan knob ([-1, 1]; 0 = center). Drag
/// horizontally to pan, double-tap to re-center; optionally shows an
/// `L##`/`C`/`R##` label below.
pub struct StudioPanKnob<'a, M> {
    value: f64,
    on_changed: Box<dyn Fn(f64) -> M + 'a>,
    on_commit: M,
    size: f32,
    show_label: bool,
    accent: Color,
}

/// A 30 px knob with its label, in the coral accent.
pub fn studio_pan_knob<'a, M>(
    value: f64,
    on_changed: impl Fn(f64) -> M + 'a,
    on_commit: M,
) -> StudioPanKnob<'a, M> {
    StudioPanKnob {
        value,
        on_changed: Box::new(on_changed),
        on_commit,
        size: 30.0,
        show_label: true,
        accent: Studio::ACCENT,
    }
}

impl<M> StudioPanKnob<'_, M> {
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn show_label(mut self, show: bool) -> Self {
        self.show_label = show;
        self
    }

    pub fn accent(mut self, accent: Color) -> Self {
        self.accent = accent;
        self
    }
}

/// A knob's drag state: the recognizer, and the pan a held drag has reached.
#[derive(Debug, Default)]
pub struct KnobState {
    gesture: PanDoubleTap,
    /// The pan a held drag has reached. Each move adds to THIS rather than
    /// to the knob's `value`, which only catches up on the next view: two
    /// pointer moves inside one frame (a fast mouse) would otherwise both
    /// start from the same stale value, and the first would be lost. Seeded
    /// from `value` per drag.
    drag: Option<f64>,
}

impl<'a, M: Clone + 'a> From<StudioPanKnob<'a, M>> for Element<'a, M> {
    fn from(knob: StudioPanKnob<'a, M>) -> Self {
        let size = knob.size;
        let mut content = column![canvas_widget(PanKnobPainter {
            value: knob.value,
            accent: knob.accent,
        })
        .width(size)
        .height(size)]
        .align_x(Horizontal::Center);
        if knob.show_label {
            content = content.push(iced::widget::Space::new().height(2)).push(
                studio_text(pan_label(knob.value))
                    .size(9)
                    .font(font::SEMIBOLD)
                    .tabular()
                    .color(Studio::TEXT_SECONDARY)
                    .width(size + 8.0)
                    .align_x(Horizontal::Center),
            );
        }
        let value = knob.value;
        let on_changed = knob.on_changed;
        let on_commit = knob.on_commit;
        Wrap::<M, KnobState>::new(
            content,
            Box::new(move |state, event, bounds, cursor, shell| {
                let now = Instant::now();
                match event {
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                        if let Some(p) = cursor.position_over(bounds) {
                            state.gesture.press(p, now);
                            shell.capture_event();
                        }
                    }
                    Event::Mouse(mouse::Event::CursorMoved { position }) => {
                        match state.gesture.moved(*position) {
                            Some(Gesture::PanStart) => state.drag = Some(value),
                            Some(Gesture::PanUpdate(delta)) => {
                                let next =
                                    pan_after_drag(state.drag.unwrap_or(value), f64::from(delta.x));
                                state.drag = Some(next);
                                shell.publish(on_changed(next));
                            }
                            _ => {}
                        }
                    }
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                        match state.gesture.release(now) {
                            Some(Gesture::PanEnd) => {
                                state.drag = None;
                                shell.publish(on_commit.clone());
                            }
                            Some(Gesture::DoubleTap) => {
                                shell.publish(on_changed(0.0));
                                shell.publish(on_commit.clone());
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }),
        )
        .interaction(Box::new(|_, bounds, cursor| {
            cursor
                .is_over(bounds)
                .then_some(mouse::Interaction::ResizingHorizontally)
        }))
        .into()
    }
}

/// Paints the knob face: a raised disc, a strong rim and the indicator.
pub struct PanKnobPainter {
    pub value: f64,
    pub accent: Color,
}

/// The indicator's direction for a pan: [-1, 1] maps to [-135°, +135°] from
/// straight up, as `(sin, -cos)`.
pub fn pan_indicator(value: f64) -> Vector {
    let angle = value.clamp(-1.0, 1.0) * (3.0 * std::f64::consts::PI / 4.0);
    Vector::new(angle.sin() as f32, -(angle.cos() as f32))
}

impl<M> canvas::Program<M> for PanKnobPainter {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let c = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let r = bounds.width / 2.0 - 1.0;
        let disc = Path::circle(c, r);
        frame.fill(&disc, Studio::SURFACE_RAISED);
        frame.stroke(
            &disc,
            Stroke::default()
                .with_color(Studio::BORDER_STRONG)
                .with_width(1.5),
        );
        let dir = pan_indicator(self.value);
        frame.stroke(
            &Path::line(c, c + dir * (r - 3.0)),
            Stroke::default()
                .with_color(self.accent)
                .with_width(2.0)
                .with_line_cap(LineCap::Round),
        );
        vec![frame.into_geometry()]
    }
}

// --------------------------------------------------------------------- fader

/// The fader's height.
pub const FADER_HEIGHT: f32 = 16.0;

/// The level a fader `width` wide reads at `dx` from its left edge.
pub fn fader_value_at(dx: f32, width: f32) -> f64 {
    (f64::from(dx) / f64::from(width)).clamp(0.0, 1.0)
}

/// `StudioFader`: a thin horizontal volume fader (track line, filled
/// portion, thumb) as wide as its parent allows and 16 px tall.
///
/// It takes its pointer on contact: a click jumps the level and commits it,
/// and a press held still before it slides stays the fader's (as a tap racing
/// a drag, a half-second hold on a take's fader lost both to the lane's
/// long-press reorder — the level had already jumped on the press, and was
/// never saved). Every move while held sets the level from the pointer's x;
/// the release commits.
pub fn studio_fader<'a, M: Clone + 'a>(
    value: f64,
    accent: Color,
    on_changed: impl Fn(f64) -> M + 'a,
    on_change_end: M,
) -> Element<'a, M> {
    canvas_widget(Fader {
        value: value.clamp(0.0, 1.0),
        accent,
        on_changed: Box::new(on_changed),
        on_change_end,
    })
    .width(Length::Fill)
    .height(FADER_HEIGHT)
    .into()
}

struct Fader<'a, M> {
    value: f64,
    accent: Color,
    on_changed: Box<dyn Fn(f64) -> M + 'a>,
    on_change_end: M,
}

/// Whether a fader drag is in flight: it has moved the level and owes its
/// change-end.
#[derive(Debug, Default)]
pub struct FaderState {
    active: bool,
}

impl<M: Clone> canvas::Program<M> for Fader<'_, M> {
    type State = FaderState;

    fn update(
        &self,
        state: &mut FaderState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<M>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                state.active = true;
                Some(
                    canvas::Action::publish((self.on_changed)(fader_value_at(p.x, bounds.width)))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if state.active => {
                Some(canvas::Action::publish((self.on_changed)(fader_value_at(
                    position.x - bounds.x,
                    bounds.width,
                ))))
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.active =>
            {
                state.active = false;
                Some(canvas::Action::publish(self.on_change_end.clone()))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &FaderState,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let mid = bounds.height / 2.0;
        let x = self.value as f32 * bounds.width;
        let line = |color| {
            Stroke::default()
                .with_color(color)
                .with_width(3.0)
                .with_line_cap(LineCap::Round)
        };
        frame.stroke(
            &Path::line(Point::new(0.0, mid), Point::new(bounds.width, mid)),
            line(Studio::GRID_STRONG),
        );
        frame.stroke(
            &Path::line(Point::new(0.0, mid), Point::new(x, mid)),
            line(self.accent),
        );
        let thumb = Path::circle(Point::new(x, mid), 5.0);
        frame.fill(&thumb, self.accent);
        frame.stroke(
            &thumb,
            Stroke::default()
                .with_color(Studio::BACKGROUND)
                .with_width(1.5),
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &FaderState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}

// ------------------------------------------------------------------ readouts

/// `FixedWidthReadout(text, width: ..., style: ...)`: a value read out in a
/// fixed width beside its control, right-aligned, so the control keeps its
/// length as the value moves. One line always: a reading the font sets wider
/// than the box is scaled down (`FittedBox(fit: scaleDown)`), not wrapped
/// onto a second line ("1444" over "ms").
pub struct FixedWidthReadout {
    text: String,
    width: f32,
    size: f32,
    font: Font,
    color: Color,
    tabular: bool,
}

/// A readout of `text` `width` px wide, in a `size` px regular face of
/// `Studio.textSecondary` with proportional figures until restyled.
pub fn fixed_width_readout(text: impl Into<String>, width: f32, size: f32) -> FixedWidthReadout {
    FixedWidthReadout {
        text: text.into(),
        width,
        size,
        font: font::REGULAR,
        color: Studio::TEXT_SECONDARY,
        tabular: false,
    }
}

impl FixedWidthReadout {
    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// `fontFeatures: [FontFeature.tabularFigures()]`.
    pub fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }

    /// The reading laid out at `size` px, as `studio_text` sets it.
    fn line_at(&self, size: f32) -> (Arc<Buffer>, Size) {
        studio_text::line(
            &self.text,
            size,
            LineHeight::default(),
            self.font,
            self.tabular,
        )
    }
}

/// The laid-out reading: its buffer, and its extent.
#[derive(Debug, Default, Clone)]
pub struct ReadoutState {
    buffer: Option<Arc<Buffer>>,
    extent: Size,
}

impl<M> Widget<M, Theme, Renderer> for FixedWidthReadout {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ReadoutState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ReadoutState::default())
    }

    fn layout(&mut self, tree: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        let (mut buffer, mut extent) = self.line_at(self.size);
        if extent.width > self.width && extent.width > 0.0 {
            (buffer, extent) = self.line_at(self.size * self.width / extent.width);
        }
        *tree.state.downcast_mut::<ReadoutState>() = ReadoutState {
            buffer: Some(buffer),
            extent,
        };
        layout::Node::new(limits.resolve(
            Length::Fixed(self.width),
            Length::Shrink,
            Size::new(self.width, extent.height),
        ))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<ReadoutState>();
        let Some(buffer) = &state.buffer else {
            return;
        };
        let bounds = layout.bounds();
        studio_text::draw_line(
            renderer,
            buffer,
            Point::new(bounds.x + bounds.width - state.extent.width, bounds.y),
            self.color,
            *viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_ref::<ReadoutState>();
        let bounds = layout.bounds();
        let drawn = Rectangle::new(
            Point::new(bounds.x + bounds.width - state.extent.width, bounds.y),
            state.extent,
        );
        operation.text(None, drawn, &self.text);
    }
}

impl<'a, M: 'a> From<FixedWidthReadout> for Element<'a, M> {
    fn from(readout: FixedWidthReadout) -> Self {
        Element::new(readout)
    }
}

/// `FaderDbReadout.format`: a fader's linear gain in dB ("-14.0"; "0.0" from
/// -0.05 dB up; "-∞" at or below 0.0001).
pub fn fader_db(volume: f64) -> String {
    if volume <= 0.0001 {
        return "-∞".to_string();
    }
    let db = 20.0 * volume.ln() / std::f64::consts::LN_10;
    if db >= -0.05 {
        "0.0".to_string()
    } else {
        to_string_as_fixed(db, 1)
    }
}

/// The width of a [`fader_db_readout`]: in 40 px, every level below -10 dB
/// wrapped onto two lines in a font as wide as the system one; this box fits.
pub const FADER_DB_READOUT_WIDTH: f32 = 46.0;

/// `FaderDbReadout(volume)`: "-14.0 dB" in a 46 px box, 10 px w600
/// `Studio.textSecondary`, tabular figures.
pub fn fader_db_readout<'a, M: 'a>(volume: f64) -> Element<'a, M> {
    fixed_width_readout(
        format!("{} dB", fader_db(volume)),
        FADER_DB_READOUT_WIDTH,
        10.0,
    )
    .font(font::SEMIBOLD)
    .color(Studio::TEXT_SECONDARY)
    .tabular()
    .into()
}

// -------------------------------------------------------------------- toggle

/// How long [`studio_toggle`]'s thumb and color take to change.
pub const TOGGLE_ANIMATION: Duration = Duration::from_millis(120);

/// `StudioToggle`: an iOS-style on/off toggle used for mute/enable in the
/// mixer — 34 × 19, a 15 px white thumb, `activeColor` when on. The thumb
/// slides and the color changes over 120 ms, linear (its
/// `AnimatedContainer`).
pub fn studio_toggle<'a, M: Clone + 'a>(
    value: bool,
    on_changed: impl Fn(bool) -> M + 'a,
    active_color: Color,
) -> Element<'a, M> {
    let thumb =
        container(iced::widget::Space::new().width(15).height(15)).style(|_| container::Style {
            background: Some(Background::Color(Color::WHITE)),
            border: Border {
                radius: 7.5.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let track = animated_container(thumb, TOGGLE_ANIMATION)
        .width(34)
        .height(19)
        .padding(2)
        .align((if value { 1.0 } else { -1.0 }, 0.0))
        .decoration(Decoration::fill(
            if value {
                active_color
            } else {
                Studio::TOGGLE_OFF
            },
            10.0,
        ));
    button(track)
        .padding(0)
        .style(bare_button)
        .on_press(on_changed(!value))
        .into()
}

// ------------------------------------------------------------- icon buttons

/// `StudioRoundButton`: a round transport button (play / stop / record),
/// `size` (default 40) across; `on_tap: None` disables it.
pub struct StudioRoundButton<M> {
    icon: Icon,
    on_tap: Option<M>,
    color: Color,
    filled: bool,
    size: f32,
}

pub fn studio_round_button<M>(icon: Icon, on_tap: Option<M>) -> StudioRoundButton<M> {
    StudioRoundButton {
        icon,
        on_tap,
        color: Studio::ACCENT,
        filled: false,
        size: 40.0,
    }
}

impl<M> StudioRoundButton<M> {
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl<'a, M: Clone + 'a> From<StudioRoundButton<M>> for Element<'a, M> {
    fn from(b: StudioRoundButton<M>) -> Self {
        let enabled = b.on_tap.is_some();
        let (fill, border, glyph) = if b.filled {
            (
                alpha(b.color, if enabled { 1.0 } else { 0.3 }),
                None,
                Studio::BACKGROUND,
            )
        } else {
            (
                Studio::SURFACE_RAISED,
                Some(Studio::BORDER_STRONG),
                alpha(b.color, if enabled { 1.0 } else { 0.35 }),
            )
        };
        let size = b.size;
        let face = container(icons::icon(b.icon, size * 0.42, glyph))
            .center_x(size)
            .center_y(size)
            .style(move |_| container::Style {
                background: Some(Background::Color(fill)),
                border: Border {
                    color: border.unwrap_or(Color::TRANSPARENT),
                    width: if border.is_some() { 1.0 } else { 0.0 },
                    radius: (size / 2.0).into(),
                },
                shadow: Shadow::default(),
                ..container::Style::default()
            });
        button(face)
            .padding(0)
            .style(bare_button)
            .on_press_maybe(b.on_tap)
            .into()
    }
}

/// `StudioMiniButton`: a small bordered icon button used in track headers
/// (24 × 22, radius 4, a 13 px icon in `color`, default
/// `Studio.textSecondary`).
pub fn studio_mini_button<'a, M: Clone + 'a>(
    icon: Icon,
    on_tap: M,
    color: Color,
) -> Element<'a, M> {
    let face = container(icons::icon(icon, 13.0, color))
        .center_x(24)
        .center_y(22)
        .style(|_| {
            crate::ui::core::theme::boxed(Some(Studio::SURFACE_RAISED), 4.0, Some(Studio::BORDER))
        });
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{center, container, row};
    use iced_test::simulator;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Pan(f64),
        Commit,
        Level(f64),
        End,
        Toggle(bool),
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }
    fn move_to(x: f32, y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(x, y),
        })
    }

    /// Drives a knob the way every strip wires it: `on_changed` feeds the new
    /// value straight back in — but only at the next view, so moves inside
    /// one frame see the same stale `value`.
    fn knob_at(value: f64) -> Element<'static, Msg> {
        center(studio_pan_knob(value, Msg::Pan, Msg::Commit)).into()
    }

    fn pans(messages: impl Iterator<Item = Msg>) -> Vec<f64> {
        messages
            .filter_map(|m| match m {
                Msg::Pan(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    // Regression guard: each move panned from the widget's value, which only
    // updates on rebuild, so two pointer moves inside one frame (a fast
    // mouse) both started from the same stale value and the first was lost.
    #[test]
    fn two_moves_inside_one_frame_both_count() {
        let mut ui = simulator(knob_at(0.0));
        let c = Point::new(512.0, 384.0);
        ui.point_at(c);
        // Press and move far enough to win the pan (that move only starts it),
        // then two moves before any rebuild.
        let _ = ui.simulate([
            press(),
            move_to(c.x + 20.0, c.y),
            move_to(c.x + 28.0, c.y),
            move_to(c.x + 36.0, c.y),
            release(),
        ]);
        let messages: Vec<_> = ui.into_messages().collect();
        let p = pans(messages.clone().into_iter());
        assert!((p.last().unwrap() - 16.0 / 80.0).abs() < 1e-9);
        assert_eq!(messages.last(), Some(&Msg::Commit));
    }

    #[test]
    fn each_drag_starts_from_the_value_as_it_is_now() {
        let c = Point::new(512.0, 384.0);
        let mut ui = simulator(knob_at(0.0));
        ui.point_at(c);
        let _ = ui.simulate([
            press(),
            move_to(c.x + 20.0, c.y),
            move_to(c.x + 36.0, c.y),
            release(),
        ]);
        let first = pans(ui.into_messages());
        assert!((first.last().unwrap() - 16.0 / 80.0).abs() < 1e-9);

        // Changed elsewhere between drags (the inspector's knob for the same
        // channel, a sync): the next drag must not resume the last one's pan.
        let mut ui = simulator(knob_at(-0.5));
        ui.point_at(c);
        let _ = ui.simulate([
            press(),
            move_to(c.x - 20.0, c.y),
            move_to(c.x - 28.0, c.y),
            release(),
        ]);
        let second = pans(ui.into_messages());
        assert!((second.last().unwrap() - (-0.5 - 8.0 / 80.0)).abs() < 1e-9);
    }

    #[test]
    fn a_double_tap_re_centers_and_commits() {
        let c = Point::new(512.0, 384.0);
        let mut ui = simulator(knob_at(0.7));
        ui.point_at(c);
        let _ = ui.simulate([press(), release()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = ui.simulate([press(), release()]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Pan(0.0), Msg::Commit]
        );
    }

    #[test]
    fn the_label_reads_left_center_right() {
        assert_eq!(pan_label(0.0), "C");
        assert_eq!(pan_label(0.004), "C");
        assert_eq!(pan_label(-0.25), "L25");
        assert_eq!(pan_label(1.0), "R100");
        assert_eq!(pan_label(0.005), "R1"); // rounds half away from zero
    }

    #[test]
    fn the_indicator_sweeps_270_degrees() {
        let up = pan_indicator(0.0);
        assert!(up.x.abs() < 1e-6 && (up.y + 1.0).abs() < 1e-6);
        let hard_right = pan_indicator(1.0);
        let r = std::f32::consts::FRAC_1_SQRT_2;
        assert!((hard_right.x - r).abs() < 1e-5 && (hard_right.y - r).abs() < 1e-5);
        assert_eq!(pan_indicator(3.0), pan_indicator(1.0));
    }

    fn fader() -> Element<'static, Msg> {
        // A 200 px fader at the window's left edge, as the Dart test sized it.
        container(row![container(studio_fader(
            0.5,
            Color::from_rgb(1.0, 0.0, 0.0),
            Msg::Level,
            Msg::End
        ))
        .width(200)])
        .into()
    }

    // A two-finger trackpad swipe pans the timeline; with the cursor over a
    // lane header's fader it reached the fader as a pan-zoom gesture and
    // jumped the take's volume.
    #[test]
    fn a_two_finger_trackpad_swipe_leaves_the_level_alone() {
        use crate::ui::core::widgets::pan_zoom::testing as pan_zoom;

        let mut ui = simulator(fader());
        ui.point_at(Point::new(100.0, 8.0));
        let swipe = pan_zoom::swipe(&[(-40.0, 0.0), (-80.0, 0.0)], Instant::now());
        pan_zoom::simulate(&mut ui, swipe);
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn a_mouse_drag_still_sets_the_level() {
        let mut ui = simulator(fader());
        ui.point_at(Point::new(100.0, 8.0));
        let _ = ui.simulate([press(), move_to(120.0, 8.0), move_to(140.0, 8.0), release()]);
        let messages: Vec<_> = ui.into_messages().collect();
        let levels: Vec<f64> = messages
            .iter()
            .filter_map(|m| {
                if let Msg::Level(v) = m {
                    Some(*v)
                } else {
                    None
                }
            })
            .collect();
        assert!(!levels.is_empty());
        assert!(*levels.last().unwrap() > 0.5);
        assert_eq!(messages.iter().filter(|m| **m == Msg::End).count(), 1);
    }

    #[test]
    fn a_click_jumps_the_level_and_commits_it() {
        let mut ui = simulator(fader());
        ui.point_at(Point::new(50.0, 8.0));
        let _ = ui.simulate([press(), release()]);
        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0], Msg::Level(v) if (v - 0.25).abs() < 0.01));
        assert_eq!(messages[1], Msg::End);
    }

    #[test]
    fn a_drag_past_the_ends_clamps() {
        assert_eq!(fader_value_at(-30.0, 200.0), 0.0);
        assert_eq!(fader_value_at(250.0, 200.0), 1.0);
        assert!((fader_value_at(50.0, 200.0) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn fader_db_reads_like_dart() {
        assert_eq!(fader_db(1.0), "0.0");
        assert_eq!(fader_db(0.2), "-14.0");
        assert_eq!(fader_db(0.00011), "-79.2");
        assert_eq!(fader_db(0.0), "-∞");
        assert_eq!(fader_db(0.0001), "-∞");
        assert_eq!(fader_db(0.9945), "0.0"); // -0.048 dB
    }

    /// The readout's text bounds, the readout boxed at the left edge.
    fn readout_text_bounds(readout: Element<'static, Msg>, text: &str) -> Rectangle {
        let mut ui = simulator(container(row![readout]));
        ui.find(text).expect("shown").bounds()
    }

    // In its 40 px box every level below -10 dB wrapped onto two lines
    // ("-14.0" over "dB") in a font as wide as the system one.
    #[test]
    fn the_db_readout_reads_on_one_line_inside_its_width_at_any_level() {
        for (volume, text) in [
            (1.0, "0.0 dB"),
            (0.2, "-14.0 dB"),
            (0.00011, "-79.2 dB"),
            (0.0, "-∞ dB"),
        ] {
            let drawn = readout_text_bounds(fader_db_readout(volume), text);
            assert!(drawn.x >= -0.01, "{text}: {drawn:?}");
            assert!(
                ((drawn.x + drawn.width) - FADER_DB_READOUT_WIDTH).abs() < 0.01,
                "{text}"
            );
            assert!(drawn.height < 10.0 * 1.3 * 1.5, "{text} is one line");
        }
    }

    // The FX panel's 42 px value box: "1444 ms" (a delay time) set 42.3 px
    // wide in a font as wide as the system one and wrapped onto two lines.
    #[test]
    fn a_reading_wider_than_its_box_scales_down_onto_one_line() {
        let wide = fixed_width_readout("1444 ms 1444 ms", 42.0, 10.5);
        let drawn = readout_text_bounds(wide.into(), "1444 ms 1444 ms");
        assert!(drawn.x >= -0.01);
        assert!(((drawn.x + drawn.width) - 42.0).abs() < 0.05);
        assert!(drawn.height < 10.5 * 1.3, "scaled down, not wrapped");
    }

    #[test]
    fn the_toggle_flips_its_value() {
        let mut ui = simulator(container(row![studio_toggle(
            false,
            Msg::Toggle,
            Studio::ACCENT
        )]));
        ui.point_at(Point::new(17.0, 9.0));
        let _ = ui.simulate([press(), release()]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Toggle(true)]
        );
    }

    #[test]
    fn the_toggle_slides_its_thumb_and_changes_color_over_120_ms() {
        use crate::ui::core::motion::Lerp;
        use crate::ui::core::widgets::animated::testing::{near, Frames, MS};
        use iced::time::Instant;

        let view = |value| studio_toggle(value, Msg::Toggle, Studio::ACCENT);
        let mut window = Frames::new(Size::new(40.0, 20.0));
        // The thumb's middle off (9.5), halfway (17) and on (24.5); the
        // track beside it.
        let (left, middle, right) = (
            Point::new(6.0, 9.0),
            Point::new(17.0, 9.0),
            Point::new(28.0, 9.0),
        );
        let at = |w: &Frames, p: Point, c: Color| near(w.pixel(p), c);
        let t0 = Instant::now();
        window.frame(view(false), &[], t0);
        assert!(at(&window, left, Color::WHITE) && at(&window, right, Studio::TOGGLE_OFF));
        assert!(window.frame(view(true), &[], t0));
        assert!(at(&window, left, Color::WHITE), "start");
        window.frame(view(true), &[], t0 + 60 * MS);
        assert!(at(&window, middle, Color::WHITE), "the thumb halfway");
        let track = Studio::TOGGLE_OFF.lerp(Studio::ACCENT, 0.5);
        assert!(at(&window, left, track), "the color halfway");
        assert!(!window.frame(view(true), &[], t0 + 120 * MS));
        assert!(
            at(&window, right, Color::WHITE) && at(&window, left, Studio::ACCENT),
            "end"
        );
    }

    #[test]
    fn a_round_button_without_a_tap_is_disabled() {
        let mut ui = simulator(container(row![Element::from(studio_round_button::<Msg>(
            Icon::Play,
            None
        ))]));
        ui.point_at(Point::new(20.0, 20.0));
        let _ = ui.simulate([press(), release()]);
        assert!(ui.into_messages().next().is_none());

        let mut ui = simulator(container(row![Element::from(studio_round_button(
            Icon::Play,
            Some(Msg::Commit)
        ))]));
        ui.point_at(Point::new(20.0, 20.0));
        let _ = ui.simulate([press(), release()]);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Commit]);
    }
}
