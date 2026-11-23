//! Port of `ui/song_practice_detail/widgets/bottom_dock.dart`: the
//! collapsible bottom dock — one tab per analysis feature (spectrogram,
//! power, vowel, pitch, timbre, range, dynamics) plus the mixer, sharing the
//! timeline/playhead with the arrangement above. The spectral/pitch/timbre
//! tabs also show the live mic via the Take/Live toggle. Collapsed by
//! default.
//!
//! Plain data in ([`BottomDock`]: what the Dart dock read off the view
//! model, and the content the screen builds for the open tab with
//! [`dock_content`]), [`Message`]s out, each named after the call the Dart
//! dock made: `vm.analysis.showDockTab`, `toggleDock`, `setLiveMonitor`,
//! `setDockHeight`, and the tabs' own messages.
//!
//! How the Dart structure maps:
//!
//! - The post-frame `healSelectedAnalysis` in `BottomDock.build`: a view
//!   cannot act, so [`heals`] says when the screen asks the cache again,
//!   after each update while it holds (as Dart asked after each build).
//! - `StableSubtree` + `ValueKey('dock-tabs')` around the tab strip: the
//!   strip keeps its tree slot (and its scroll) as the resize handle comes and
//!   goes above it, because the handle and the content keep slots of their
//!   own, empty while the dock is closed.
//! - `maxHeight`: the screen hands the dock what the workspace has left once
//!   the timeline keeps a usable height; the content shows at
//!   [`content_height`].
//! - The resize handle's `onVerticalDragUpdate`: a drag (past the mouse's
//!   1 px hit slop, the movement up to it swallowed) reports each move's
//!   delta with the cap the dock was built under
//!   ([`Message::Resize`]); the screen sets the height from the height ON
//!   SCREEN, read per move (`min(dockHeight, cap) - dy`), so two moves landing
//!   before a rebuild both count, and a drag down from a capped dock moves it
//!   at once.
//!
//! Where iced differs: text has no letter spacing (the tabs' 0.8, set
//! solid); the tab strip scrolls without a visible scrollbar.

#[cfg(test)]
mod tests;

use iced::alignment::Vertical;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{button, column, container, row, scrollable, stack, text, Row, Space};
use iced::{mouse, Color, Element, Event, Length, Padding};
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};

use super::left_panel::{self, voice_summary_view, VoiceSummary};
use super::mixer_strip::{self, MixerStrip};
use super::voice_profile_view::{dynamics_view, range_view};
use super::voiceprint_view::{self as voiceprint, VoiceprintState};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{bare_button, boxed, fill, font, Studio};
use crate::ui::core::widgets::wrap::Wrap;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::DockTab;

/// The resize grabber's height.
pub const HANDLE_HEIGHT: f32 = 10.0;

/// The tab strip's height.
pub const TAB_BAR_HEIGHT: f32 = 34.0;

/// The dock's chrome over its content: the top border, the resize grabber
/// and the tab bar (`_chromeHeight`).
pub const CHROME_HEIGHT: f32 = 1.0 + HANDLE_HEIGHT + TAB_BAR_HEIGHT;

/// How far a mouse moves vertically before the grabber's drag wins
/// (`kPrecisePointerHitSlop`).
const DRAG_SLOP: f32 = 1.0;

/// Short tab label for each dock feature (`_tabLabel`).
pub fn tab_label(t: DockTab) -> &'static str {
    match t {
        DockTab::AnalysisSummary => "Analysis Summary",
        DockTab::Spectrogram => "Spectrogram",
        DockTab::Power => "Power",
        DockTab::Ltas => "LTAS",
        DockTab::Vowel => "Vowel",
        DockTab::Pitch => "Pitch",
        DockTab::Vibrato => "Vibrato",
        DockTab::Timbre => "Timbre",
        DockTab::Registers => "Register",
        DockTab::Range => "Range",
        DockTab::Dynamics => "Dynamics",
        DockTab::Mixer => "Mixer",
    }
}

/// The content height on screen (`_contentHeight`): as dragged
/// (`dock_height`), but never more than the workspace has room for
/// (`max_height`, the whole dock's share, chrome included) — and never
/// negative.
pub fn content_height(dock_height: f64, max_height: f32) -> f32 {
    (dock_height as f32)
        .min(max_height - CHROME_HEIGHT)
        .max(0.0)
}

/// The height a resize move sets (`setDockHeight(_contentHeight - dy)`):
/// from the height on screen — the stored height, capped at `cap` (the
/// content's room) — less the move.
pub fn resized_height(dock_height: f64, cap: f32, dy: f32) -> f64 {
    dock_height.min(f64::from(cap)).max(0.0) - f64::from(dy)
}

/// Whether the dock asks the analysis cache again for what its tab shows,
/// once the screen has updated (`healSelectedAnalysis` after the frame): the
/// dock is open on a tab that reads the take, not on the live mic, and the
/// cache let some of it go (`selectedAnalysisIncomplete`).
pub fn heals(open: bool, shows_take: bool, live_monitor: bool, incomplete: bool) -> bool {
    open && shows_take && !live_monitor && incomplete
}

/// What the dock asks of the screen.
#[derive(Debug, Clone)]
pub enum Message {
    /// A tab: `vm.analysis.showDockTab(tab)` (opens the dock on it).
    ShowDockTab(DockTab),
    /// The panel toggle: `vm.analysis.toggleDock()`.
    ToggleDock,
    /// A move of the resize grabber's drag, `dy` px (down is positive),
    /// with the content's room `cap` as the dock was built: set the height
    /// to [`resized_height`].
    Resize { dy: f32, cap: f32 },
    /// A voiceprint tab, or the Take/Live toggle (`setLiveMonitor`).
    Voiceprint(voiceprint::Message),
    /// The Analysis Summary tab.
    Summary(left_panel::Message),
    /// The Mixer tab.
    Mixer(mixer_strip::Message),
}

/// What the dock shows: the view-model state `BottomDock` read.
pub struct BottomDock<'a> {
    /// `vm.analysis.dockOpen`.
    pub open: bool,
    /// `vm.analysis.dockTab`.
    pub tab: DockTab,
    /// `vm.analysis.activeTabSupportsLive`: the Take/Live toggle shows (while
    /// open).
    pub supports_live: bool,
    /// `vm.liveMonitor`.
    pub live: bool,
    /// `vm.analysis.dockHeight`: the content's height as dragged.
    pub dock_height: f64,
    /// The most the whole dock may take: what the workspace has left once
    /// the timeline keeps a usable height. The dragged height is clamped to
    /// a fixed range, not to the window, so on a window shorter than the dock
    /// at its tallest a drag up overflowed the workspace, collapsed the
    /// timeline and cut off the dock's bottom.
    pub max_height: f32,
    /// The open tab's content ([`dock_content`]); shown while open.
    pub content: Option<Element<'a, Message>>,
}

/// `BottomDock`: the resize grabber (open), the tab strip, and the open
/// tab's content at [`content_height`].
pub fn bottom_dock(dock: BottomDock<'_>) -> Element<'_, Message> {
    let open = dock.open;
    let cap = dock.max_height - CHROME_HEIGHT;
    let height = content_height(dock.dock_height, dock.max_height);
    // The handle and the content keep their slots while closed (empty), so
    // the strip between them keeps its own (and its scroll) as the dock
    // opens: unkeyed, Dart rebuilt the strip from scratch, its scroll back
    // at the start just as a tab past the window's edge opened the dock.
    let handle: Element<'_, Message> = if open {
        resize_handle(cap)
    } else {
        Space::new().into()
    };
    let content: Element<'_, Message> = match (open, dock.content) {
        // The content's top border is painted over it (a `DecoratedBox`
        // does not inset its child).
        (true, Some(content)) => container(stack![
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .clip(true),
            column![container(Space::new())
                .width(Length::Fill)
                .height(1)
                .style(fill(Studio::BORDER))],
        ])
        .width(Length::Fill)
        .height(height)
        .into(),
        _ => Space::new().into(),
    };
    let tabs = tab_bar(open, dock.tab, open && dock.supports_live, dock.live);
    column![
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
        container(column![handle, tabs, content].width(Length::Fill))
            .width(Length::Fill)
            .style(fill(Studio::SURFACE)),
    ]
    .width(Length::Fill)
    .into()
}

/// The open tab's content, as `BottomDock` switched over `dockTab`: the
/// voice summary, a voiceprint tab over `src` (the Spectrogram tab's take
/// image is `voiceprint`'s), Range or Dynamics, or the mixer desk.
pub fn dock_content<'a>(
    tab: DockTab,
    src: &'a dyn AnalysisSource,
    voiceprint_state: &'a VoiceprintState,
    summary: VoiceSummary<'a>,
    mixer: MixerStrip<'a>,
) -> Element<'a, Message> {
    let tab_view = |e: Element<'a, voiceprint::Message>| e.map(Message::Voiceprint);
    match tab {
        DockTab::AnalysisSummary => voice_summary_view(summary).map(Message::Summary),
        DockTab::Spectrogram => tab_view(voiceprint::spectrogram_tab(src, voiceprint_state)),
        DockTab::Power => tab_view(voiceprint::power_tab(src)),
        DockTab::Ltas => tab_view(voiceprint::ltas_tab(src)),
        DockTab::Vowel => tab_view(voiceprint::vowel_tab(src)),
        DockTab::Pitch => tab_view(voiceprint::pitch_tab(src)),
        DockTab::Vibrato => tab_view(voiceprint::vibrato_tab(src)),
        DockTab::Timbre => tab_view(voiceprint::timbre_tab(src)),
        DockTab::Registers => tab_view(voiceprint::register_tab(src)),
        DockTab::Range => range_view(src),
        DockTab::Dynamics => dynamics_view(src),
        DockTab::Mixer => mixer.view().map(Message::Mixer),
    }
}

/// The subject's analysis the voice summary shows, as the screen read it
/// off the cache (the summary borrows it).
#[derive(Debug, Clone, Default)]
pub struct SummaryData {
    pub spectrogram_loading: bool,
    pub metrics_loading: bool,
    pub metrics: Option<std::sync::Arc<VoiceMetrics>>,
    pub profile: Option<std::sync::Arc<VoiceProfile>>,
    pub register: Option<std::sync::Arc<RegisterMap>>,
    pub ltas: Option<std::sync::Arc<Ltas>>,
}

impl SummaryData {
    /// The summary of `selected_track` over this analysis.
    pub fn summary<'a>(
        &'a self,
        selected_track: Option<&'a vocara_model::models::audio_track::AudioTrack>,
    ) -> VoiceSummary<'a> {
        VoiceSummary {
            selected_track,
            spectrogram_loading: self.spectrogram_loading,
            metrics_loading: self.metrics_loading,
            metrics: self.metrics.as_ref(),
            profile: self.profile.as_deref(),
            register: self.register.as_deref(),
            ltas: self.ltas.as_deref(),
        }
    }
}

/// `_TabBar`: the analysis tabs (scrolling on a narrow window), the
/// Take/Live toggle where the open tab has one, the mixer tab set apart, and
/// the dock toggle.
fn tab_bar<'a>(open: bool, tab: DockTab, shows_live: bool, live: bool) -> Element<'a, Message> {
    let mut tabs = Row::new().align_y(Vertical::Center);
    for t in DockTab::ALL {
        if t == DockTab::Mixer {
            continue;
        }
        tabs = tabs
            .push(dock_tab(
                tab_label(t),
                open && tab == t,
                Message::ShowDockTab(t),
            ))
            .push(Space::new().width(6));
    }
    let mut bar = row![
        Space::new().width(12),
        // Left: the voice-analysis tabs (scroll on a narrow window).
        scrollable(tabs)
            .direction(Direction::Horizontal(
                Scrollbar::new().width(0).scroller_width(0)
            ))
            .width(Length::Fill),
    ]
    .align_y(Vertical::Center)
    .height(TAB_BAR_HEIGHT);
    if shows_live {
        bar = bar
            .push(Space::new().width(8))
            .push(voiceprint::source_toggle(live).map(Message::Voiceprint));
    }
    // Right: Mixer, separated from the analysis tabs.
    bar = bar
        .push(Space::new().width(10))
        .push(
            container(Space::new())
                .width(1)
                .height(16)
                .style(fill(Studio::BORDER)),
        )
        .push(Space::new().width(10))
        .push(dock_tab(
            tab_label(DockTab::Mixer),
            open && tab == DockTab::Mixer,
            Message::ShowDockTab(DockTab::Mixer),
        ))
        .push(Space::new().width(6))
        .push(
            button(
                container(icons::icon(
                    // Panel-toggle icon, matching the left inspector + right
                    // lyrics panel's collapse buttons (this is the bottom
                    // panel).
                    if open {
                        Icon::PanelBottomClose
                    } else {
                        Icon::PanelBottomOpen
                    },
                    15.0,
                    Studio::TEXT_SECONDARY,
                ))
                .padding(Padding::from([0, 12])),
            )
            .padding(0)
            .style(bare_button)
            .on_press(Message::ToggleDock),
        );
    bar.into()
}

/// `_Tab`: an upper-cased 11 px label, raised with a border when active.
fn dock_tab<'a>(label: &'static str, active: bool, on_tap: Message) -> Element<'a, Message> {
    let face = container(
        text(label.to_uppercase())
            .size(11)
            .font(font::BOLD)
            .color(if active {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_FAINT
            }),
    )
    // 12 / 5 inside the 1 px border.
    .padding(Padding::from([6, 13]))
    .style(move |_| {
        boxed(
            Some(if active {
                Studio::SURFACE_RAISED
            } else {
                Color::TRANSPARENT
            }),
            5.0,
            Some(if active {
                Studio::BORDER
            } else {
                Color::TRANSPARENT
            }),
        )
    });
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

/// A press of the grabber in flight.
#[derive(Debug, Default)]
struct Grab {
    /// Where the pointer was at the press, and since its last move.
    press: Option<GrabPress>,
}

#[derive(Debug, Clone, Copy)]
struct GrabPress {
    down: f32,
    last: f32,
    dragging: bool,
}

/// `_ResizeHandle`: a thin grabber along the dock's top edge; drag it to
/// resize the dock.
fn resize_handle<'a>(cap: f32) -> Element<'a, Message> {
    let grip = container(
        container(Space::new())
            .width(36)
            .height(3)
            .style(|_| boxed(Some(Studio::BORDER_STRONG), 2.0, None)),
    )
    .width(Length::Fill)
    .height(HANDLE_HEIGHT)
    .center_x(Length::Fill)
    .center_y(HANDLE_HEIGHT);
    Wrap::<Message, Grab>::new(
        grip,
        Box::new(move |state, event, bounds, cursor, shell| match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if shell.is_event_captured() {
                    return;
                }
                if let Some(p) = cursor.position_over(bounds) {
                    state.press = Some(GrabPress {
                        down: p.y,
                        last: p.y,
                        dragging: false,
                    });
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let Some(press) = state.press.as_mut() else {
                    return;
                };
                if !press.dragging {
                    // The movement up to the slop is swallowed
                    // (`DragStartBehavior.start`).
                    if (position.y - press.down).abs() > DRAG_SLOP {
                        press.dragging = true;
                        press.last = position.y;
                    }
                    return;
                }
                let dy = position.y - press.last;
                press.last = position.y;
                if dy != 0.0 {
                    shell.publish(Message::Resize { dy, cap });
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.press = None;
            }
            _ => {}
        }),
    )
    .interaction(Box::new(|state, bounds, cursor| {
        (state.press.is_some() || cursor.is_over(bounds))
            .then_some(mouse::Interaction::ResizingVertically)
    }))
    .into()
}
