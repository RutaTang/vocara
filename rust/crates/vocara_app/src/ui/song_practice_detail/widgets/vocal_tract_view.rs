//! The dock's Vocal tract tab: how the singer's vocal tract moves, live
//! while they sing (or along the selected take): a 2D side cutaway or a 3D
//! head they can turn, and a panel of readouts.
//!
//! # The seam with the articulatory engine
//!
//! The view draws a [`TractPose`] ([`pose`]) and nothing else: plain
//! outlines in centimetres, the airway's rings, the components the panel
//! reads, the formants, the fit and whether there was a voice. The screen
//! joins the engine in ONE place, its `feed_vocal_tract` (after every update
//! while the tab is shown): it reads the engine's estimate (the live mic's
//! on Live, the selected take's at the playhead on Take), converts it with
//! the adapter (`super::vocal_tract_engine::pose_of`) and hands it to
//! [`VocalTractState::feed`]; while there is none yet (the engine getting
//! ready for the voice, a take's shapes computing) it calls
//! [`VocalTractState::feed_preparing`], and with no take to read, `feed`
//! with `None`. Nothing here knows the engine's types. The demo
//! ([`TractPose::demo`]: a singer holding "ah", "ee", "oo" and resting)
//! stays for the tests and the figure's resting shape.
//!
//! # What the tab shows
//!
//! - A header: "Vocal tract", where the shape comes from (a red dot and
//!   "Live from your microphone", or the take's name), the 2D / 3D control
//!   and the Take / Live control (the same switch as the other tabs', here
//!   in the tab's own header as the mockup has it; the strip shows none for
//!   this tab).
//! - The figure, in a well: the 2D cutaway ([`draw2d`]: fixed anatomy, the
//!   moving parts, the airway, callouts, a legend) or the 3D view
//!   ([`draw3d`]: drag to turn, scroll to zoom, Side / ¾ / Front). Both draw
//!   from centimetres at one scale fitted to the well, keeping the aspect.
//! - The readout panel ([`readouts`]).
//!
//! # Motion
//!
//! Estimates arrive irregularly (the capture pump's 30 ms, the transport's
//! frames, a seek); the figure follows each on critically damped springs
//! ([`spring`], about 70 ms), and the screen asks for frames only while
//! something still moves. An unvoiced estimate leaves the last voiced shape
//! in place, dimmed a little, and the panel says it is resting.
//!
//! # Accessibility
//!
//! iced has no accessibility tree; as elsewhere in the app, every control
//! and the figure carry a label for the widget tree ([`labelled`]): the
//! controls their names ("View", "Source", "View angle"), the figure a
//! sentence describing the pose ([`describe`]), each readout its value.

pub mod anatomy;
pub mod callouts;
pub mod camera;
pub mod demo;
pub mod draw2d;
pub mod draw3d;
pub mod geometry;
pub mod pose;
pub mod readouts;
pub mod spring;

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::time::Duration;

use iced::alignment::Vertical;
use iced::time::Instant;
use iced::widget::canvas::{self, Frame, Geometry};
use iced::widget::{canvas as canvas_widget, column, container, responsive, row, stack, Space};
use iced::{
    mouse, Background, Border, Color, Element, Length, Padding, Pixels, Point, Rectangle, Renderer,
    Theme,
};

use self::camera::{Camera, Preset};
use self::pose::{Components, Fit};
use self::readouts::{Readouts, Row, Status};
use self::spring::{PoseSpring, Spring};
use crate::ui::core::theme::{alpha, boxed, font, type_scale, Studio};
use crate::ui::core::widgets::controls::{labelled, segmented};
use crate::ui::core::widgets::studio_text::studio_text;

pub use self::pose::TractPose;

/// The figure's two views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TractView {
    #[default]
    TwoD,
    ThreeD,
}

/// What the tab reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The 2D / 3D control.
    SetView(TractView),
    /// The Take / Live control: the host's live-monitor switch (as the
    /// other tabs' `SelectLive`).
    SelectLive(bool),
    /// A drag over the 3D view, by (dx, dy) px: turn the head.
    Turn { dx: f32, dy: f32 },
    /// A scroll over the 3D view, by lines (up positive): zoom.
    Zoom(f32),
    /// A preset of the 3D view.
    Preset(Preset),
    /// A frame while something moves.
    Frame(Instant),
}

/// How long the readout bands look back (s).
pub const RANGE_WINDOW: f64 = 2.0;
/// How long the vowel map's trail is (s).
pub const TRAIL_WINDOW: f64 = 0.35;
/// How long a preset takes to turn the head (to 90 %).
pub const TURN_SETTLE: Duration = Duration::from_millis(320);
/// How far the clock may move on between two estimates before it counts as
/// a jump (a seek), across which no formant is carried (s). Estimates come
/// every frame while the playhead runs, every 30 ms on Live.
pub const CARRY_JUMP: f64 = 0.5;

/// Whose voice the estimates are (see [`VocalTractState::set_voice`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Voice {
    /// The microphone's, on Live.
    Live,
    /// The take's with this id, at the playhead.
    Take(String),
}

/// One voiced estimate, remembered for the bands and the trail.
#[derive(Clone, Copy, Debug)]
struct Sample {
    clock: f64,
    positions: [f32; 7],
    f1: f32,
    f2: f32,
}

/// The tab's own state, kept by the screen for the session: the view
/// chosen, the 3D camera, the pose on its springs and the recent estimates.
#[derive(Debug)]
pub struct VocalTractState {
    view: TractView,
    camera: [Spring; 3],
    pose: PoseSpring,
    history: VecDeque<Sample>,
    /// Whether the last feed had nothing to read (Take, no take).
    no_source: bool,
    /// Why, when it was not that there is no take: the panel's status.
    why_none: Option<Status>,
    /// When Live began (the live clock's zero).
    live_since: Option<Instant>,
    last_frame: Option<Instant>,
    /// Whose voice the estimates are now.
    voice: Option<Voice>,
    /// The formants last found in that voice, which a frame missing one
    /// keeps (see [`VocalTractState::feed`]).
    known: Option<[f32; 3]>,
    /// The clock of the last estimate.
    last_clock: Option<f64>,
}

impl Default for VocalTractState {
    fn default() -> Self {
        Self::new()
    }
}

impl VocalTractState {
    pub fn new() -> Self {
        let c = Camera::preset(Preset::ThreeQuarter);
        Self {
            view: TractView::TwoD,
            camera: [
                Spring::new(c.yaw, TURN_SETTLE),
                Spring::new(c.pitch, TURN_SETTLE),
                Spring::new(c.zoom, TURN_SETTLE),
            ],
            pose: PoseSpring::default(),
            history: VecDeque::new(),
            no_source: false,
            why_none: None,
            live_since: None,
            last_frame: None,
            voice: None,
            known: None,
            last_clock: None,
        }
    }

    pub fn view(&self) -> TractView {
        self.view
    }

    /// The 3D camera as drawn now.
    pub fn camera(&self) -> Camera {
        Camera {
            yaw: self.camera[0].value,
            pitch: self.camera[1].value,
            zoom: self.camera[2].value,
        }
    }

    fn camera_goal(&self) -> Camera {
        Camera {
            yaw: self.camera[0].goal,
            pitch: self.camera[1].goal,
            zoom: self.camera[2].goal,
        }
    }

    /// The preset the camera is at or turning to, if any.
    pub fn preset(&self) -> Option<Preset> {
        self.camera_goal().matching()
    }

    /// Handles the tab's own messages ([`Message::SelectLive`] is the
    /// host's).
    pub fn update(&mut self, message: Message, now: Instant) {
        match message {
            Message::SetView(view) => self.view = view,
            Message::SelectLive(_) => {}
            Message::Turn { dx, dy } => {
                let c = self.camera().turned(dx, dy);
                self.camera[0].snap(c.yaw);
                self.camera[1].snap(c.pitch);
            }
            Message::Zoom(lines) => {
                let z = self.camera().zoomed(lines).zoom;
                self.camera[2].snap(z);
            }
            Message::Preset(p) => {
                let goal = Camera::preset(p);
                // The shorter way round.
                let from = self.camera[0].value;
                self.camera[0].goal = from + camera::wrap(goal.yaw - from);
                self.camera[1].goal = goal.pitch;
                self.camera[2].goal = 1.0;
                self.last_frame = Some(now);
            }
            Message::Frame(at) => self.advance(at),
        }
    }

    fn advance(&mut self, now: Instant) {
        let dt = self
            .last_frame
            .map_or(0.0, |l| now.saturating_duration_since(l).as_secs_f32())
            .min(0.25);
        self.last_frame = Some(now);
        for s in &mut self.camera {
            s.advance(dt);
        }
        let yaw = &mut self.camera[0];
        if yaw.settled() {
            yaw.snap(camera::wrap(yaw.value));
        }
        self.pose.advance(now);
    }

    /// The clock an estimate is for: on Live the seconds since Live began,
    /// else the playhead's.
    pub fn clock(&mut self, live: bool, position: Duration, now: Instant) -> f64 {
        if live {
            let since = *self.live_since.get_or_insert(now);
            now.saturating_duration_since(since).as_secs_f64()
        } else {
            self.live_since = None;
            position.as_secs_f64()
        }
    }

    /// Takes in that there is no estimate yet for `clock` because it is
    /// being worked out: on Live (`live`) the engine getting ready for the
    /// voice, on Take the take's shapes computing. The figure rests.
    pub fn feed_preparing(&mut self, live: bool, clock: f64, now: Instant) {
        self.feed(None, clock, now);
        self.why_none = Some(Status::Preparing { live });
    }

    /// Takes in that the take has nothing to read (its voice could not be
    /// analyzed). The figure rests.
    pub fn feed_unavailable(&mut self, clock: f64, now: Instant) {
        self.feed(None, clock, now);
        self.why_none = Some(Status::Unavailable);
    }

    /// Takes in whose voice the estimates are from now on: Live's, or the
    /// selected take's (the screen, as it feeds). Another voice than the
    /// last carries nothing over from it ([`VocalTractState::feed`]).
    pub fn set_voice(&mut self, voice: Voice) {
        if self.voice.as_ref() != Some(&voice) {
            self.voice = Some(voice);
            self.known = None;
        }
    }

    /// Takes in the estimate for `clock` (`None`: nothing to read, Take
    /// with no take), at `now`. With nothing to read the figure goes to its
    /// resting shape, not the last one sung: with the backing selected it
    /// kept the take's shape, and the panel its readings.
    pub fn feed(&mut self, estimate: Option<TractPose>, clock: f64, now: Instant) {
        if self.last_frame.is_none() {
            self.last_frame = Some(now);
        }
        self.no_source = estimate.is_none();
        self.why_none = None;
        if self.no_source {
            self.pose.clear();
            self.known = None;
        }
        let mut pose =
            estimate.unwrap_or_else(|| demo::articulate(&demo::AH, false, Fit::Uncertain));
        // A formant the analysis did not find this frame (0: unknown) keeps
        // the last one found in the same voice: gliding it to 0 would read
        // "F2 0 Hz" and put the vowel map's dot on a vowel nobody sang. Not
        // one from another voice (Live, another take: `set_voice`), nor from
        // before a seek (the clock jumped): that vowel nobody sang here
        // either.
        let jumped = self
            .last_clock
            .is_some_and(|last| clock < last || clock - last > CARRY_JUMP);
        self.last_clock = Some(clock);
        if jumped {
            self.known = None;
        }
        if let Some(known) = self.known {
            for (f, k) in pose.formants.iter_mut().zip(known) {
                if f.is_nan() || *f <= 0.0 {
                    *f = k;
                }
            }
        }
        if pose.voiced {
            self.known = Some(pose.formants);
        }
        // The bands and the trail look back from this clock (a seek back
        // forgets what came after it).
        while self
            .history
            .back()
            .is_some_and(|s| s.clock > clock || s.clock < clock - RANGE_WINDOW)
        {
            self.history.pop_back();
        }
        while self
            .history
            .front()
            .is_some_and(|s| s.clock < clock - RANGE_WINDOW)
        {
            self.history.pop_front();
        }
        if pose.voiced {
            let sample = Sample {
                clock,
                positions: readouts::positions(&pose.components),
                f1: pose.formants[0],
                f2: pose.formants[1],
            };
            // The same moment again (a parked playhead, fed on every
            // refresh) replaces its sample rather than piling up.
            match self.history.back_mut() {
                Some(last) if last.clock == clock => *last = sample,
                _ => self.history.push_back(sample),
            }
        }
        self.pose.push(pose, now);
    }

    /// Whether anything still moves: the screen asks for frames while so.
    pub fn animating(&self) -> bool {
        self.pose.animating() || self.camera.iter().any(|s| !s.settled())
    }

    /// The pose on screen (the demo's resting "ah" before any estimate).
    pub fn shown(&self) -> TractPose {
        self.pose
            .shown()
            .unwrap_or_else(|| demo::articulate(&demo::AH, false, Fit::Uncertain))
    }

    /// 0 voiced to 1 resting.
    pub fn rest(&self) -> f32 {
        self.pose.rest()
    }

    /// Whether the figure rests (no voice, or nothing to read).
    pub fn resting(&self) -> bool {
        !self.pose.voiced() || self.no_source
    }

    /// Each component's range over the last two seconds, along its track.
    pub fn bands(&self) -> [Option<(f32, f32)>; 7] {
        let mut out = [None; 7];
        for s in &self.history {
            for (band, p) in out.iter_mut().zip(s.positions) {
                *band = Some(match *band {
                    Some((lo, hi)) => (f32::min(lo, p), f32::max(hi, p)),
                    None => (p, p),
                });
            }
        }
        out
    }

    /// The vowel map's trail (F1, F2), oldest first.
    pub fn trail(&self) -> Vec<(f32, f32)> {
        let Some(last) = self.history.back() else {
            return Vec::new();
        };
        self.history
            .iter()
            .filter(|s| s.clock >= last.clock - TRAIL_WINDOW)
            .filter(|s| s.f1 > 0.0 && s.f2 > 0.0)
            .map(|s| (s.f1, s.f2))
            .collect()
    }

    /// Forgets the estimates (the page let go of its analysis).
    pub fn clear(&mut self) {
        self.pose.clear();
        self.history.clear();
        self.live_since = None;
        self.voice = None;
        self.known = None;
        self.last_clock = None;
    }

    /// What the panel shows now.
    pub fn readouts(&self) -> Readouts {
        let pose = self.shown();
        let resting = self.resting();
        let positions = readouts::positions(&pose.components);
        let values = readouts::values(&pose.components);
        let bands = self.bands();
        let rows = readouts::ROWS
            .iter()
            .enumerate()
            .map(|(i, &(label, lo, hi))| Row {
                label,
                lo,
                hi,
                position: positions[i],
                band: if resting { None } else { bands[i] },
                // Nothing to read: no readings, rather than a shape's.
                value: if self.no_source || (resting && i == 6) {
                    "\u{2014}".to_string()
                } else {
                    values[i].clone()
                },
            })
            .collect();
        let status = if self.no_source {
            self.why_none.unwrap_or(Status::NoTake)
        } else if resting {
            Status::Resting
        } else {
            Status::Voiced {
                fit: pose.fit,
                formants: pose.formants,
            }
        };
        Readouts {
            rows,
            vowel: (!self.no_source && pose.formants[0] > 0.0 && pose.formants[1] > 0.0)
                .then_some((pose.formants[0], pose.formants[1])),
            trail: if resting { Vec::new() } else { self.trail() },
            status,
            rest: if self.no_source { 1.0 } else { self.rest() },
        }
    }
}

/// Where the shape comes from, for the header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source<'a> {
    Live,
    /// A take: its name and colour.
    Take(&'a str, Color),
    /// Take mode with no take selected.
    NoTake,
}

/// What the tab shows.
pub struct VocalTract<'a> {
    pub state: &'a VocalTractState,
    /// Whether the dock is on Live.
    pub live: bool,
    pub source: Source<'a>,
}

/// The header's height and the gap under it.
const HEADER: f32 = 22.0;
const HEADER_GAP: f32 = 8.0;

/// The tab (see the module docs).
pub fn vocal_tract_view(v: VocalTract<'_>) -> Element<'_, Message> {
    let state = v.state;
    let header = header(v.live, v.source, state.view());
    let well = well(state);
    let left = column![header, well]
        .spacing(HEADER_GAP)
        .width(Length::Fill)
        .height(Length::Fill);
    let readouts = state.readouts();
    let panel = container(responsive(move |size| {
        readouts::panel(&readouts, size.height)
    }))
    .width(readouts::WIDTH)
    .height(Length::Fill);
    container(row![left, panel].spacing(16).height(Length::Fill))
        .padding(Padding {
            top: 12.0,
            right: 16.0,
            bottom: 14.0,
            left: 16.0,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn label_text<'a>(s: impl Into<std::borrow::Cow<'a, str>>, color: Color) -> Element<'a, Message> {
    studio_text(s)
        .size(type_scale::LABEL)
        .line_height(Pixels(type_scale::LINE))
        .color(color)
        .into()
}

/// A round dot of `size` px in `color`, with a `ring` px halo at `halo`.
fn dot<'a>(color: Color, size: f32, ring: f32, halo: f32) -> Element<'a, Message> {
    let outer = size + 2.0 * ring;
    stack![
        container(
            container(Space::new())
                .width(outer)
                .height(outer)
                .style(move |_: &Theme| boxed(Some(alpha(color, halo)), outer / 2.0, None))
        )
        .center(outer),
        container(
            container(Space::new())
                .width(size)
                .height(size)
                .style(move |_: &Theme| boxed(Some(color), size / 2.0, None))
        )
        .center(outer),
    ]
    .into()
}

/// The source line: "Live from your microphone" or the take.
pub fn source_text(source: Source<'_>) -> String {
    match source {
        Source::Live => "Live from your microphone".to_string(),
        Source::Take(name, _) => name.to_string(),
        Source::NoTake => "No take selected".to_string(),
    }
}

fn header<'a>(live: bool, source: Source<'a>, view: TractView) -> Element<'a, Message> {
    let title = studio_text("Vocal tract")
        .size(type_scale::BODY)
        .line_height(Pixels(type_scale::LINE))
        .font(font::SEMIBOLD)
        .color(Studio::TEXT_PRIMARY);
    let from: Element<'a, Message> = match source {
        Source::Live => row![
            dot(Studio::RECORD, 7.0, 3.0, 0.18),
            label_text(source_text(source), Studio::TEXT_SECONDARY)
        ]
        .spacing(3)
        .align_y(Vertical::Center)
        .into(),
        Source::Take(_, color) => row![
            dot(color, 7.0, 3.0, 0.0),
            label_text(source_text(source), Studio::TEXT_SECONDARY)
        ]
        .spacing(3)
        .align_y(Vertical::Center)
        .into(),
        Source::NoTake => label_text(source_text(source), Studio::TEXT_FAINT),
    };
    let views = labelled(
        segmented(
            &[(TractView::TwoD, "2D"), (TractView::ThreeD, "3D")],
            view,
            Message::SetView,
        ),
        "View",
    );
    let sources = labelled(
        segmented(
            &[(false, "Take"), (true, "Live")],
            live,
            Message::SelectLive,
        ),
        "Source",
    );
    row![
        title,
        Space::new().width(10),
        from,
        Space::new().width(Length::Fill),
        views,
        Space::new().width(10),
        sources,
    ]
    .align_y(Vertical::Center)
    .height(HEADER)
    .into()
}

/// The well's corner radius and its outline.
const WELL_RADIUS: f32 = 10.0;
const WELL_EDGE: Color = Color::from_rgb8(0x22, 0x22, 0x28);

/// The figure's well: the figure, its legend and notes, the 3D presets, and
/// the outline over all.
fn well(state: &VocalTractState) -> Element<'_, Message> {
    let pose = state.shown();
    let rest = state.rest();
    let label = describe(
        &pose,
        state.view(),
        state.camera().matching(),
        state.resting(),
    );
    let figure: Element<'_, Message> = match state.view() {
        TractView::TwoD => canvas_widget(Figure2d { pose, rest })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        TractView::ThreeD => canvas_widget(Figure3d {
            pose,
            rest,
            camera: state.camera(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    };
    let figure = container(labelled(figure, label))
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(|_: &Theme| boxed(Some(draw2d::WELL_BASE), WELL_RADIUS, None));
    let small = |s: &'static str, color: Color| -> Element<'_, Message> {
        studio_text(s)
            .size(type_scale::AXIS)
            .line_height(Pixels(type_scale::LINE))
            .color(color)
            .into()
    };
    let swatch = |background: Background, radius: f32, border: Option<(Color, f32)>| {
        container(Space::new())
            .width(8)
            .height(8)
            .style(move |_: &Theme| container::Style {
                background: Some(background),
                border: Border {
                    color: border.map_or(Color::TRANSPARENT, |b| b.0),
                    width: border.map_or(0.0, |b| b.1),
                    radius: radius.into(),
                },
                ..container::Style::default()
            })
    };
    let item = |sw: Element<'static, Message>, s: &'static str| -> Element<'static, Message> {
        row![sw, small(s, Studio::TEXT_FAINT)]
            .spacing(6)
            .align_y(Vertical::Center)
            .into()
    };
    let corner =
        |e: Element<'static, Message>, left: bool, top: bool| -> Element<'static, Message> {
            let c = container(e)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(Padding {
                    top: 10.0,
                    right: 14.0,
                    bottom: 12.0,
                    left: 14.0,
                });
            let c = if left {
                c.align_left(Length::Fill)
            } else {
                c.align_right(Length::Fill)
            };
            if top {
                c.align_top(Length::Fill)
            } else {
                c.align_bottom(Length::Fill)
            }
            .into()
        };
    let mut layers: Vec<Element<'_, Message>> = vec![figure.into()];
    match state.view() {
        TractView::TwoD => {
            let moves = Background::Gradient(iced::Gradient::Linear(
                iced::gradient::Linear::new(std::f32::consts::PI)
                    .add_stop(0.0, draw2d::TONGUE_TOP)
                    .add_stop(1.0, draw2d::TONGUE_FOOT),
            ));
            let legend = row![
                item(swatch(moves, 2.0, None).into(), "Moves as you sing"),
                item(
                    swatch(Background::Color(draw2d::BONE_TOP), 2.0, None).into(),
                    "Bone"
                ),
                item(
                    swatch(Background::Color(alpha(Studio::IN_TUNE, 0.45)), 2.0, None).into(),
                    "Airway"
                ),
            ]
            .spacing(14)
            .align_y(Vertical::Center);
            layers.push(corner(legend.into(), true, false));
            layers.push(corner(
                small("Drawn to scale \u{b7} adult model", Studio::TEXT_DISABLED),
                false,
                false,
            ));
        }
        TractView::ThreeD => {
            let legend = row![
                small("Drag to turn \u{b7} scroll to zoom", Studio::TEXT_FAINT),
                item(
                    swatch(
                        Background::Color(Color::from_rgb8(0xE3, 0xA0, 0x94)),
                        2.0,
                        None
                    )
                    .into(),
                    "Tongue"
                ),
                item(
                    swatch(
                        Background::Color(Color::TRANSPARENT),
                        4.0,
                        Some((alpha(Studio::ACCENT_BRIGHT, 0.8), 1.5))
                    )
                    .into(),
                    "Airway"
                ),
            ]
            .spacing(14)
            .align_y(Vertical::Center);
            layers.push(corner(legend.into(), true, false));
            let presets: Vec<(Option<Preset>, &str)> =
                Preset::ALL.iter().map(|p| (Some(*p), p.label())).collect();
            let control = labelled(
                segmented(&presets, state.preset(), |p| {
                    Message::Preset(p.unwrap_or(Preset::ThreeQuarter))
                }),
                "View angle",
            );
            layers.push(corner(control, false, true));
        }
    }
    layers.push(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| container::Style {
                border: Border {
                    color: WELL_EDGE,
                    width: 1.0,
                    radius: WELL_RADIUS.into(),
                },
                ..container::Style::default()
            })
            .into(),
    );
    iced::widget::Stack::with_children(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The 2D figure's canvas.
struct Figure2d {
    pose: TractPose,
    rest: f32,
}

impl canvas::Program<Message> for Figure2d {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw2d::draw_well_glow(&mut frame, bounds.size());
        draw2d::draw_figure(&mut frame, bounds.size(), &self.pose, self.rest);
        vec![frame.into_geometry()]
    }
}

/// The 3D figure's canvas: drag turns the head, scrolling zooms.
struct Figure3d {
    pose: TractPose,
    rest: f32,
    camera: Camera,
}

/// A drag over the 3D view in flight: where the pointer was last.
#[derive(Debug, Default)]
pub struct Drag {
    last: Option<Point>,
}

impl canvas::Program<Message> for Figure3d {
    type State = Drag;

    fn update(
        &self,
        state: &mut Drag,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                state.last = Some(p);
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let last = state.last?;
                state.last = Some(*position);
                let (dx, dy) = (position.x - last.x, position.y - last.y);
                (dx != 0.0 || dy != 0.0)
                    .then(|| canvas::Action::publish(Message::Turn { dx, dy }).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.last.take().map(|_| canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_over(bounds)?;
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                (lines != 0.0).then(|| canvas::Action::publish(Message::Zoom(lines)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Drag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        draw2d::draw_well_glow(&mut frame, bounds.size());
        draw3d::draw_scene(
            &mut frame,
            bounds.size(),
            &self.pose,
            self.camera,
            self.rest,
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Drag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.last.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

/// A sentence describing the figure, for a screen reader (the figure's
/// label): which view, the kind of vowel and where each articulator is.
pub fn describe(
    pose: &TractPose,
    view: TractView,
    preset: Option<Preset>,
    resting: bool,
) -> String {
    let c: &Components = &pose.components;
    let vowel = match (c.tongue_height, c.tongue_front) {
        (h, _) if h < 0.45 => "an open vowel",
        (_, f) if f > 0.62 => "a close front vowel",
        (_, f) if f < 0.38 => "a close back vowel",
        _ => "a mid vowel",
    };
    let jaw = match c.jaw_mm {
        mm if mm >= 12.0 => "the jaw open",
        mm if mm >= 8.0 => "the jaw half open",
        _ => "the jaw nearly closed",
    };
    let height = match c.tongue_height {
        h if h < 0.4 => "low",
        h if h < 0.65 => "mid",
        _ => "high",
    };
    let place = match c.tongue_front {
        f if f > 0.62 => "forward",
        f if f > 0.5 => "central",
        f if f > 0.36 => "slightly back",
        _ => "back",
    };
    let lips = match c.lip_round {
        r if r < 0.2 => "the lips spread",
        r if r < 0.6 => "the lips apart",
        _ => "the lips rounded",
    };
    let velum = if c.velum_open < 0.2 {
        "the velum raised"
    } else {
        "the velum lowered"
    };
    let larynx = match c.larynx_mm {
        mm if mm > 4.5 => "the larynx high",
        mm if mm > 1.5 => "the larynx slightly high",
        mm if mm < -4.5 => "the larynx low",
        mm if mm < -1.5 => "the larynx slightly low",
        _ => "the larynx at rest",
    };
    let what = match view {
        TractView::TwoD => "A side cutaway of the head, to scale".to_string(),
        TractView::ThreeD => format!(
            "A 3D wireframe head, to scale, {}: the airway drawn as glowing rings from the vocal folds to the lips, the tongue as ribs",
            match preset {
                Some(Preset::Side) => "seen from the side",
                Some(Preset::ThreeQuarter) => "turned to three quarters",
                Some(Preset::Front) => "seen from the front",
                None => "turned by hand",
            }
        ),
    };
    let doing = if resting {
        format!("resting on the last shape sung, {vowel}")
    } else {
        format!("while singing {vowel}")
    };
    format!("{what}, {doing}: {jaw}, the tongue {height} and {place}, {lips}, {velum} and {larynx}")
}
