//! Port of `ui/song_practice_detail/widgets/bottom_dock.dart`: the
//! collapsible bottom dock — one tab per analysis feature (spectrogram,
//! power, vowel, pitch, timbre, range, dynamics), the vocal tract (new, not
//! in the Dart app: `vocal_tract_view`) plus the mixer, sharing the
//! timeline/playhead with the arrangement above. The spectral/pitch/timbre
//! tabs and the vocal tract also show the live mic via the Take/Live toggle.
//! Collapsed by default.
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
//! Where iced differs: the tab strip scrolls without a visible scrollbar.
//!
//! The look is `DESIGN.md`'s (Tabs, and the approved workspace and mixer
//! mockups): a 32 px `PANEL` strip of sentence-case dock tabs in groups —
//! Summary | Vocal tract | Pitch · Vibrato · Range · Register |
//! Spectrogram · Power · LTAS · Timbre · Vowel · Dynamics ([`STRIP_GROUPS`];
//! `DockTab`'s own order is unchanged) — then the Take/Live toggle where the
//! open tab has one (the vocal tract carries its own in its header, as its
//! mockup has it: [`strip_shows_source`]), the Mixer tab with its glyph at
//! the right end and the show/hide chevron. Open,
//! a quiet 6 px grabber sits above the strip and the content below it, on
//! the dock's [`CONTENT_BACKGROUND`] under a 1 px rule.

#[cfg(test)]
mod tests;

use crate::ui::core::widgets::fading_scrollbar::scrollable;
use iced::alignment::Vertical;
use iced::widget::{column, container, row, stack, Space};
use iced::{mouse, Color, Element, Event, Length, Padding};
use vocara_model::models::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};

use super::left_panel::{self, voice_summary_view, VoiceSummary};
use super::mixer_strip::{self, MixerStrip};
use super::vocal_tract_view::{self, vocal_tract_view, VocalTract};
use super::voice_profile_view::{dynamics_view, range_view};
use super::voiceprint_view::{self as voiceprint, VoiceprintState};
use crate::ui::core::theme::{boxed, fill, Studio};
use crate::ui::core::widgets::controls::{icon_button, tab, tab_strip, Tab, TabSize};
use crate::ui::core::widgets::line_icon::glyphs;
use crate::ui::core::widgets::wrap::Wrap;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::DockTab;

/// The resize grabber's height.
pub const HANDLE_HEIGHT: f32 = 6.0;

/// The tab strip's height.
pub const TAB_BAR_HEIGHT: f32 = 32.0;

/// The dock's chrome over its content: the top border, the resize grabber
/// and the tab bar (`_chromeHeight`).
pub const CHROME_HEIGHT: f32 = 1.0 + HANDLE_HEIGHT + TAB_BAR_HEIGHT;

/// What the open dock's content lies on (the mixer mockup's dock body):
/// a shade under the strip's `PANEL`, so the cards and wells on it read.
pub const CONTENT_BACKGROUND: Color = Color::from_rgb8(0x13, 0x13, 0x16);

/// The 1 px rule between the open strip and its content (the mockup's).
const STRIP_RULE: Color = Color::from_rgb8(0x22, 0x22, 0x28);

/// The analysis tabs as the strip shows them, group by group (`DESIGN.md`,
/// Tabs; the vocal tract in a group of its own after Summary, as its mockup
/// shows); the Mixer tab sits apart at the strip's right end.
pub const STRIP_GROUPS: [&[DockTab]; 4] = [
    &[DockTab::AnalysisSummary],
    &[DockTab::VocalTract],
    &[
        DockTab::Pitch,
        DockTab::Vibrato,
        DockTab::Range,
        DockTab::Registers,
    ],
    &[
        DockTab::Spectrogram,
        DockTab::Power,
        DockTab::Ltas,
        DockTab::Timbre,
        DockTab::Vowel,
        DockTab::Dynamics,
    ],
];

/// The show / hide chevron's tooltip (and label), as the dock is now.
pub fn toggle_label(open: bool) -> &'static str {
    if open {
        "Hide analysis panel"
    } else {
        "Show analysis panel"
    }
}

/// How far a mouse moves vertically before the grabber's drag wins
/// (`kPrecisePointerHitSlop`).
const DRAG_SLOP: f32 = 1.0;

/// Short tab label for each dock feature (`_tabLabel`), in sentence case.
pub fn tab_label(t: DockTab) -> &'static str {
    match t {
        DockTab::AnalysisSummary => "Summary",
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
        DockTab::VocalTract => "Vocal tract",
        DockTab::Mixer => "Mixer",
    }
}

/// Whether the strip shows the Take/Live toggle for `tab` (while open, where
/// the tab supports Live): every such tab but the vocal tract, which has the
/// toggle in its own header.
pub fn strip_shows_source(tab: DockTab, supports_live: bool) -> bool {
    supports_live && tab != DockTab::VocalTract
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
    /// The Vocal tract tab.
    VocalTract(vocal_tract_view::Message),
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
        // The rule under the strip is painted over the content's top (a
        // `DecoratedBox` does not inset its child).
        (true, Some(content)) => container(stack![
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .clip(true),
            column![container(Space::new())
                .width(Length::Fill)
                .height(1)
                .style(fill(STRIP_RULE))],
        ])
        .width(Length::Fill)
        .height(height)
        .style(fill(CONTENT_BACKGROUND))
        .into(),
        _ => Space::new().into(),
    };
    let tabs = tab_bar(
        open,
        dock.tab,
        open && strip_shows_source(dock.tab, dock.supports_live),
        dock.live,
    );
    column![
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
        container(column![handle, tabs, content].width(Length::Fill))
            .width(Length::Fill)
            .style(fill(Studio::PANEL)),
    ]
    .width(Length::Fill)
    .into()
}

/// The open tab's content, as `BottomDock` switched over `dockTab`: the
/// voice summary, a voiceprint tab over `src` (the Spectrogram tab's take
/// image is `voiceprint`'s), Range or Dynamics, the vocal tract, or the
/// mixer desk.
pub fn dock_content<'a>(
    tab: DockTab,
    src: &'a dyn AnalysisSource,
    voiceprint_state: &'a VoiceprintState,
    summary: VoiceSummary<'a>,
    mixer: MixerStrip<'a>,
    vocal_tract: VocalTract<'a>,
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
        DockTab::VocalTract => vocal_tract_view(vocal_tract).map(Message::VocalTract),
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

/// `_TabBar`: the analysis tabs in their groups (scrolling on a narrow
/// window), the Take/Live toggle where the open tab has one, the Mixer tab
/// at the right end, and the show / hide chevron. No tab reads as selected
/// while the dock is closed.
fn tab_bar<'a>(open: bool, tab: DockTab, shows_live: bool, live: bool) -> Element<'a, Message> {
    let selected = open.then_some(tab);
    let mut tabs: Vec<Tab<'a, DockTab>> = Vec::new();
    for (g, group) in STRIP_GROUPS.iter().enumerate() {
        for (i, &t) in group.iter().enumerate() {
            let entry = self::tab(t, tab_label(t));
            tabs.push(if g > 0 && i == 0 {
                entry.divided()
            } else {
                entry
            });
        }
    }
    let analysis = tab_strip(tabs, selected, TabSize::Dock, Message::ShowDockTab);
    let mut bar = row![
        // Left: the voice-analysis tabs (scroll on a narrow window).
        scrollable(analysis).horizontal().width(Length::Fill),
        Space::new().width(8),
    ]
    .align_y(Vertical::Center)
    .padding(Padding::from([0, 8]))
    .height(TAB_BAR_HEIGHT);
    if shows_live {
        bar = bar
            .push(voiceprint::source_toggle(live).map(Message::Voiceprint))
            .push(
                container(
                    container(Space::new())
                        .width(1)
                        .height(14)
                        .style(fill(Studio::BORDER_STRONG)),
                )
                .padding(Padding::from([0, 8])),
            );
    }
    // Right: Mixer, apart from the analysis tabs, then the chevron.
    bar = bar
        .push(tab_strip(
            vec![self::tab(DockTab::Mixer, tab_label(DockTab::Mixer)).icon(glyphs::MIXER)],
            selected,
            TabSize::Dock,
            Message::ShowDockTab,
        ))
        .push(Space::new().width(2))
        .push(
            icon_button(
                if open {
                    glyphs::CHEVRON_DOWN
                } else {
                    glyphs::CHEVRON_UP
                },
                Message::ToggleDock,
            )
            .panel()
            .icon_size(12.0)
            .tooltip(toggle_label(open)),
        );
    bar.into()
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

/// `_ResizeHandle`: a thin grabber along the dock's top edge, a quiet 28 × 2
/// `GRID_STRONG` grip centred in it; drag it to resize the dock.
fn resize_handle<'a>(cap: f32) -> Element<'a, Message> {
    let grip = container(
        container(Space::new())
            .width(28)
            .height(2)
            .style(|_| boxed(Some(Studio::GRID_STRONG), 1.0, None)),
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
