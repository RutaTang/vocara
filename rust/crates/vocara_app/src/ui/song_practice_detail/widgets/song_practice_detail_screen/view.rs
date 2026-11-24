//! The page's view: the bar shown while there is no workspace, the
//! workspace (transport, inspector, the arrange, the right panel, the dock)
//! and its overlays (the catch-up guard, the lyrics Focus view, the
//! separation progress, the note editor, the technique picker, the snack
//! bar).

use std::convert::Infallible;
use std::sync::Arc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, gradient, Frame, Geometry, Path};
use iced::widget::{
    button, canvas as canvas_widget, column, container, mouse_area, opaque, responsive, row,
    scrollable, stack, text, Space,
};
use iced::{
    keyboard, mouse, Color, Element, Event, Length, Padding, Point, Rectangle, Renderer, Shadow,
    Size, Theme, Vector,
};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_num::round_to_i64;

use super::{
    ms, selected_track_ref, DropZone, InterpretationAction, Message, OpenMenu, Shown,
    SongPracticeDetailScreen, FINISHING_SAVE, MIN_TIMELINE_HEIGHT, NOT_LOADED, WAIT_FOR_SAVE,
    WAIT_FOR_SEPARATION,
};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::Curve;
use crate::ui::core::theme::{self, alpha, bare_button, boxed, fill, font, Studio};
use crate::ui::core::widgets::animated::{animated, animated_container, Decoration};
use crate::ui::core::widgets::holds_keyboard::holds_keyboard;
use crate::ui::core::widgets::slide::inert;
use crate::ui::core::widgets::studio_button::studio_button;
use crate::ui::core::widgets::studio_controls::{fader_db_readout, studio_fader};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::visible_drop_target::visible_drop_target;
use crate::ui::core::widgets::waveform_painter::{PlayheadPainter, TimelineGridPainter};
use crate::ui::core::widgets::wrap::Wrap;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::song_practice_detail::view_model::interpretations_controller::InterpretationsController;
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::{
    LyricsMode, RightTab, SongPracticeDetailViewModel,
};
use crate::ui::song_practice_detail::widgets::analysis_lanes::{analysis_lanes, LaneImage};
use crate::ui::song_practice_detail::widgets::bottom_dock::{self, bottom_dock, BottomDock};
use crate::ui::song_practice_detail::widgets::detail_transport_bar::{
    detail_transport_bar, TransportBar,
};
use crate::ui::song_practice_detail::widgets::fx_panel::{EqInput, FxPanel, FxTarget};
use crate::ui::song_practice_detail::widgets::left_panel::{left_panel, Inspector};
use crate::ui::song_practice_detail::widgets::lyrics_panel::{
    self, lyrics_focus_overlay, right_panel, RightPanelData,
};
use crate::ui::song_practice_detail::widgets::mixer_shared::keyed_column;
use crate::ui::song_practice_detail::widgets::mixer_strip::{MicInput, MixerStrip};
use crate::ui::song_practice_detail::widgets::ruler_bar::{ruler_bar, RulerBar, RULER_HEIGHT};
use crate::ui::song_practice_detail::widgets::timeline::{
    timeline_envelope_painter, timeline_scroll_thumb,
};
use crate::ui::song_practice_detail::widgets::track_context_menu::{
    argb, bus_context_menu, clip_context_menu, menu_host, studio_menu, track_context_menu, BusMenu,
    StudioMenuItem, TrackMenu,
};
use crate::ui::song_practice_detail::widgets::track_row::{
    ms_toggle, track_menu_button, track_row, TIMELINE_H_MARGIN, TRACK_HEADER_W, TRACK_ROW_HEIGHT,
};

/// What `vm.lyricsAsText()` gives a view (only the update side reads it).
static NO_TEXT: fn() -> String = String::new;

/// The scroll a wheel's line moves (iced's own scrollable's).
const LINE_PX: f32 = 60.0;

/// A lane of the timeline list, by what it shows (Dart's keys: the take and
/// bus rows by id, the master by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneKey {
    BackingHeader,
    Backing,
    TakesHeader,
    NewTakeZone,
    Take(u64),
    BusesHeader,
    Bus(u64),
    MasterHeader,
    Master,
}

fn id_key(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

/// What the right panel reads of the view model, as it is now.
pub(super) fn panel_data<'a>(
    vm: &'a SongPracticeDetailViewModel,
    shown: &'a Shown,
    lyrics_as_text: &'a dyn Fn() -> String,
    under_dialog: bool,
) -> RightPanelData<'a> {
    let position_ms = ms(vm.position());
    RightPanelData {
        lyrics_open: vm.lyrics_open(),
        right_tab: match vm.right_tab() {
            RightTab::Lyrics => lyrics_panel::RightTab::Lyrics,
            RightTab::Notes => lyrics_panel::RightTab::Notes,
            RightTab::Techniques => lyrics_panel::RightTab::Techniques,
        },
        lyrics_mode: match vm.lyrics_mode() {
            LyricsMode::Read => lyrics_panel::LyricsMode::Read,
            LyricsMode::Edit => lyrics_panel::LyricsMode::Edit,
            LyricsMode::Sync => lyrics_panel::LyricsMode::Sync,
        },
        lyrics_focus: vm.lyrics_focus(),
        has_lyrics: vm.has_lyrics(),
        lyrics: vm.lyrics(),
        lyrics_as_text,
        lyrics_draft: vm.lyrics_draft(),
        lyrics_edit_session: vm.lyrics_edit_session(),
        sync_index: vm.sync_index() as i64,
        current_lyric_index: vm.current_lyric_index(position_ms).map_or(-1, |i| i as i64),
        selected_track: vm
            .practice()
            .and_then(|p| selected_track_ref(vm, p))
            .map(|t| &**t),
        selected_track_notes: &shown.notes,
        selected_track_techniques: &shown.techniques,
        all_notes: &shown.all_notes,
        under_dialog,
        revision: vm.revision(),
    }
}

impl Shown {
    /// What the views borrow, read off the view model now.
    pub(super) fn read(&mut self, vm: &SongPracticeDetailViewModel) {
        let practice = vm.practice();
        self.vocal_takes = InterpretationsController::vocal_takes(practice);
        // Only the active backing (per the Accompaniment / Original toggle)
        // is shown — it's the only one that's audible, so the mixer mirrors
        // the timeline.
        self.mixer_tracks = vm
            .active_backing()
            .into_iter()
            .chain(
                practice
                    .map(SongPractice::active_vocal_tracks)
                    .unwrap_or_default(),
            )
            .collect();
        self.notes = vm.selected_track_notes();
        self.techniques = vm.selected_track_techniques();
        self.all_notes = vm
            .all_notes()
            .into_iter()
            .map(|e| lyrics_panel::NoteRef {
                track_id: e.track_id,
                track_name: e.track_name,
                note: e.note,
            })
            .collect();
        self.summary = vm.with_analysis(|a, h| bottom_dock::SummaryData {
            spectrogram_loading: a.spectrogram_loading(h),
            metrics_loading: a.metrics_loading(h),
            metrics: a.selected_metrics(h),
            profile: a.selected_profile(h),
            register: a.selected_register(h),
            ltas: a.selected_ltas(h),
        });
    }
}

impl SongPracticeDetailScreen {
    /// Whether something is over the page (a dialog, the editors, a menu):
    /// Dart's `!ModalRoute.isCurrentOf(context)`.
    fn under_dialog(&self) -> bool {
        self.dialogs_open > 0
            || self.note_editor.is_some()
            || self.picker.is_some()
            || self.menu.is_some()
    }

    /// The page: no workspace while loading or when the practice could not
    /// be loaded; else the workspace and its overlays.
    pub(super) fn page(&self) -> Element<'_, Message> {
        let vm = &*self.vm;
        if vm.load_running() && vm.practice().is_none() {
            return no_workspace(spinner(36.0, 4.0, Studio::TEXT_SECONDARY));
        }
        let Some(practice) = vm.practice() else {
            return no_workspace(
                text(NOT_LOADED)
                    .size(14)
                    .color(Studio::TEXT_SECONDARY)
                    .into(),
            );
        };
        let workspace = responsive(move |size| {
            // However tall the dock is dragged, the timeline keeps its
            // place.
            let max_dock = size.height - MIN_TIMELINE_HEIGHT;
            column![
                row![
                    self.left_panel_view(practice),
                    container(self.center(practice))
                        .width(Length::Fill)
                        .height(Length::Fill),
                    self.right_panel_view(),
                ]
                .height(Length::Fill),
                self.dock_view(practice, max_dock),
            ]
            .into()
        });
        let content = column![
            detail_transport_bar(self.transport_bar(practice)).map(Message::Transport),
            catch_up_guard(workspace.into(), vm.catching_up()),
        ];
        // Every overlay keeps its slot, empty while it is not shown: iced
        // matches a stack's layers by position, so a layer coming or going
        // moved the ones above it and made them anew — a snack bar's did the
        // note editor's and the picker's, whose field dropped the keyboard
        // mid-word. (Dart's editors were dialogs of their own.)
        let focus = vm.lyrics_focus().then(|| {
            let data = panel_data(vm, &self.shown, &NO_TEXT, self.under_dialog());
            lyrics_focus_overlay(&self.lyrics, data).map(Message::Lyrics)
        });
        let separation = vm
            .is_separating()
            .then(|| separation_overlay(vm.separation_progress(), vm.can_cancel_separation()));
        // A closed one takes the open one's slot as it goes, taking no input.
        let now = self.dialog_now;
        let editor = match (&self.note_editor, &self.closing_editor) {
            (Some((editor, _)), _) => Some(
                editor
                    .view_at(self.editor_motion.fade(now))
                    .map(Message::NoteEditor),
            ),
            (None, Some(going)) => Some(inert(
                going.0.view_at(going.1.fade(now)).map(Message::NoteEditor),
            )),
            (None, None) => None,
        };
        let picker = match (&self.picker, &self.closing_picker) {
            (Some(open), _) => Some(
                open.picker
                    .view_at(self.picker_motion.fade(now))
                    .map(Message::TechniquePicker),
            ),
            (None, Some(going)) => Some(inert(
                going
                    .0
                    .view_at(going.1.fade(now))
                    .map(Message::TechniquePicker),
            )),
            (None, None) => None,
        };
        stack![
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(fill(Studio::BACKGROUND)),
            slot(focus),
            slot(separation),
            slot(
                self.snacks
                    .view()
                    .map(|snacks| snacks.map(|never| match never {})),
            ),
            slot(editor),
            slot(picker),
        ]
        .into()
    }

    fn transport_bar<'a>(&'a self, practice: &'a SongPractice) -> TransportBar<'a> {
        let vm = &*self.vm;
        let playback = vm.playback();
        TransportBar {
            practice,
            visible_note_count: vm.with_mixer(|m, h| m.visible_note_count(h)),
            is_playing: playback.is_playing(),
            is_recording: vm.is_recording(),
            is_starting_take: vm.recording().is_starting_take(),
            loop_enabled: playback.loop_enabled(),
            has_loop: playback.has_loop(),
            follow_cursor: playback.follow_cursor(),
            timeline_zoom: vm.with_playback(|p, h| p.timeline_zoom(h)),
            position: vm.position(),
            input_monitor_enabled: vm.recording().input_monitor_enabled(),
            busy: vm.busy(),
        }
    }

    /// `LeftPanel`: the inspector, its FX slot filled with the shown
    /// channel's `FxPanel`.
    fn left_panel_view<'a>(&'a self, practice: &'a SongPractice) -> Element<'a, Message> {
        let vm = &*self.vm;
        let selected_track = selected_track_ref(vm, practice).map(|t| &**t);
        let selected_bus = vm.selected_bus().map(|b| &**b);
        let inspector = Inspector {
            open: vm.inspector_open(),
            selected_track,
            selected_bus,
            fx_buses: &practice.fx_buses,
            supports_effects: vm.mixer().supports_effects(),
            vocal_takes: &self.shown.vocal_takes,
            active_interpretation: practice.active_version().map(|v| v.name.as_str()),
            open_menu: match &self.menu {
                Some(OpenMenu::Inspector(menu)) => Some(menu),
                _ => None,
            },
        };
        let shown = &self.shown;
        let fx_panel = move || -> Element<'a, Message> {
            let target = match (selected_bus, selected_track) {
                (Some(bus), _) => FxTarget::Bus {
                    id: &bus.id,
                    fx: &bus.fx,
                },
                (None, Some(track)) => FxTarget::Track {
                    id: &track.id,
                    fx: track.fx.as_ref(),
                },
                (None, None) => return Space::new().into(),
            };
            FxPanel {
                target,
                eq_input: shown.eq_input.as_ref().map(|c| EqInput {
                    col: c.col(),
                    bin_hz: c.bin_hz(),
                }),
                gain_reduction_db: shown.gain_reduction_db,
            }
            .view()
            .map(Message::Fx)
        };
        left_panel(inspector, fx_panel, Message::Inspector)
    }

    fn right_panel_view(&self) -> Element<'_, Message> {
        let data = panel_data(&self.vm, &self.shown, &NO_TEXT, self.under_dialog());
        right_panel(&self.lyrics, data).map(Message::Lyrics)
    }

    /// `BottomDock`, at most `max_height` tall.
    fn dock_view<'a>(
        &'a self,
        practice: &'a SongPractice,
        max_height: f32,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let a = vm.analysis();
        let open = a.dock_open();
        let content = open.then(|| {
            let settings = vm.app_settings();
            let mixer = MixerStrip {
                tracks: &self.shown.mixer_tracks,
                buses: &practice.fx_buses,
                supports_effects: vm.mixer().supports_effects(),
                selected_track_id: vm.selected_track_id(),
                selected_bus_id: vm.selected_bus_id(),
                master_volume: practice.master_volume,
                mic: MicInput {
                    record_gain_db: settings.record_gain_db(),
                    monitor_gain: settings.monitor_gain(),
                    monitor_allowed: settings.monitor_allowed(),
                    suggest_headphones_mark: settings.suggest_headphones_mark(),
                    input_level: vm.recording().input_level(),
                },
            };
            let selected = selected_track_ref(vm, practice).map(|t| &**t);
            bottom_dock::dock_content(
                a.dock_tab(),
                vm as &dyn AnalysisSource,
                &self.voiceprint,
                self.shown.summary.summary(selected),
                mixer,
            )
        });
        bottom_dock(BottomDock {
            open,
            tab: a.dock_tab(),
            supports_live: a.active_tab_supports_live(),
            live: a.live_monitor(),
            dock_height: a.dock_height(),
            max_height,
            content,
        })
        .map(Message::Dock)
    }

    // ------------------------------------------------------- the arrange

    /// `_Center`: the ruler over the lane list and the timeline scrollbar,
    /// with the loop band across the timeline while looping.
    fn center<'a>(&'a self, practice: &'a SongPractice) -> Element<'a, Message> {
        let vm = &*self.vm;
        let playback = vm.playback();
        // The visible timeline span (zoomable down to a 1-second window) and
        // the absolute time at the left edge; the ruler, grid, lanes, loop
        // band and master all scale to this viewport.
        let dur = vm.with_playback(|p, h| p.view_span_ms(h));
        let view_start = vm.with_playback(|p, h| p.view_start_ms(h));
        let ruler = ruler_bar(RulerBar {
            timeline_duration_ms: dur,
            view_start_ms: view_start,
            position: vm.position(),
            loop_start_ms: playback.loop_start_ms(),
            loop_end_ms: playback.loop_end_ms(),
            loop_enabled: playback.loop_enabled(),
        })
        .map(Message::Ruler);
        let list = scrollable(
            container(self.lane_list(practice, dur, view_start)).padding(Padding {
                bottom: 28.0,
                ..Padding::ZERO
            }),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::scrollbar);
        let lanes = container(pan_area(list.into(), dur))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(Studio::BACKGROUND));
        let column = column![ruler, lanes, self.scrollbar(practice, dur, view_start)];
        let mut layers = stack![column];
        // Loop region band across the timeline area (only while looping).
        if playback.loop_enabled() && dur > 0 {
            layers = layers.push(
                canvas_widget(LoopBand {
                    start_ms: playback.loop_start_ms(),
                    end_ms: playback.loop_end_ms(),
                    view_start_ms: view_start,
                    span_ms: dur,
                })
                .width(Length::Fill)
                .height(Length::Fill),
            );
        }
        layers.into()
    }

    /// The `ListView` of lanes: the backing, the vocal takes, the FX buses
    /// and the master, each keyed by what it shows.
    fn lane_list<'a>(
        &'a self,
        practice: &'a SongPractice,
        dur: i64,
        view_start: i64,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let reference = vm.mixer().play_reference();
        let mut list = keyed_column::<LaneKey, Message>()
            .width(Length::Fill)
            .push(LaneKey::BackingHeader, backing_header(reference));
        // Backing region: its own drop zone, so only it highlights when a
        // file is dragged over it. Drop fills the empty backing slot; any
        // extra files become new takes.
        let backing = if reference {
            practice.reference.as_deref()
        } else {
            practice.accompaniment.as_deref()
        };
        let backing_lane = match backing {
            Some(track) => self.lane(track, DropZone::Backing, dur, view_start, practice),
            None => self.drop_hint_zone(
                DropZone::Backing,
                if reference { Icon::Disc3 } else { Icon::Music },
                if reference {
                    "Drag the original song here, or click to browse"
                } else {
                    "Drag a backing track here, or click to browse"
                },
            ),
        };
        list = list
            .push(LaneKey::Backing, backing_lane)
            .push(LaneKey::TakesHeader, self.takes_header(practice));
        if self.shown.vocal_takes.is_empty() && !vm.is_recording() {
            list = list.push(
                LaneKey::NewTakeZone,
                self.drop_hint_zone(
                    DropZone::NewTake,
                    Icon::Mic,
                    "Record a take, drag in vocal audio, or click to browse",
                ),
            );
        } else {
            // Each take is its own drop zone: dropping audio onto a take's
            // lane adds it as a new clip on THAT take (only that lane
            // highlights). The live recording draws on the armed lane itself
            // (Logic / Cubase), so there's no separate REC lane.
            for t in &self.shown.vocal_takes {
                list = list.push(
                    LaneKey::Take(id_key(&t.id)),
                    self.lane(t, DropZone::Take(t.id.clone()), dur, view_start, practice),
                );
            }
        }
        // FX BUSES — DAW group lanes (Cubase-style), between the takes and
        // the master. Only on the native FX engine.
        if vm.mixer().supports_effects() {
            list = list.push(LaneKey::BusesHeader, buses_header());
            // Keyed by bus, as the take rows are: unkeyed, a bus deleted
            // above one whose fader was being dragged handed the rest of the
            // drag to the next.
            for b in &practice.fx_buses {
                list = list.push(
                    LaneKey::Bus(id_key(&b.id)),
                    self.bus_row(b, practice, dur, view_start),
                );
            }
        }
        // Keyed too: matched by position, a lane added or removed above
        // rebuilt the row mid-drag and the rest of the master fader's drag
        // did nothing.
        list.push(LaneKey::MasterHeader, section_label("MASTER TRACK", 18.0))
            .push(LaneKey::Master, self.master_row(practice, dur, view_start))
            .into()
    }

    /// `_Lane`: `track`'s row in its own file drop zone, highlighted while a
    /// file is dragged over it, with its lane and clip menus.
    fn lane<'a>(
        &'a self,
        track: &'a AudioTrack,
        zone: DropZone,
        dur: i64,
        view_start: i64,
        practice: &'a SongPractice,
    ) -> Element<'a, Message> {
        let hover = self.drop_hover.as_ref() == Some(&zone);
        let row = container(self.row(track, dur, view_start, practice)).padding(Padding {
            left: TIMELINE_H_MARGIN,
            right: TIMELINE_H_MARGIN,
            ..Padding::ZERO
        });
        // Always a stack, so the row keeps its widget state as the
        // highlight comes and goes.
        let mut highlighted = stack![row];
        if hover {
            highlighted = highlighted.push(
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(|_| container::Style {
                        border: iced::Border {
                            color: Studio::ACCENT,
                            width: 1.5,
                            radius: 0.0.into(),
                        },
                        ..boxed(Some(alpha(Studio::ACCENT, 0.08)), 0.0, None)
                    }),
            );
        }
        drop_zone(highlighted.into(), zone)
    }

    /// `_row(track)`: the take's `TrackRow`, its fold-out lanes, wrapped in
    /// the host its lane and clip menus pop from.
    fn row<'a>(
        &'a self,
        track: &'a AudioTrack,
        dur: i64,
        view_start: i64,
        practice: &'a SongPractice,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let id = track.id.clone();
        let selected = vm.selected_track_id() == Some(track.id.as_str());
        let target = vm.record_target_id();
        let is_target = target.as_deref() == Some(track.id.as_str());
        let expanded = vm.analysis().is_track_expanded(&track.id);
        let mut row = track_row(
            track,
            track.display_name.clone(),
            dur,
            ms(vm.position()),
            move |m| Message::Row {
                track_id: id.clone(),
                message: m,
            },
        )
        .view_start_ms(view_start)
        .selected(selected)
        .armed(!track.is_backing() && is_target)
        .selected_clip_id(if selected {
            vm.selected_clip_id()
        } else {
            None
        })
        .reorderable(!track.is_backing())
        .expand(!track.is_backing(), expanded, true)
        .lane_drag(self.lane_drag.as_deref())
        .dropping(self.drop_target.as_deref() == Some(track.id.as_str()));
        if vm.is_recording() && is_target {
            let recording = vm.recording();
            row = row.recording(recording.recording_peaks(), recording.live_take_start_ms());
        }
        if expanded {
            row = row.sub_lanes(self.lanes_view(track, dur, view_start));
        }
        let menu = match &self.menu {
            Some(OpenMenu::Track(t)) if *t == track.id => {
                Some(track_context_menu(&self.track_menu(track, practice)).map(Message::TrackMenu))
            }
            Some(OpenMenu::Clip { track_id, .. }) if *track_id == track.id => {
                Some(clip_context_menu(true).map(Message::ClipMenu))
            }
            _ => None,
        };
        menu_host(row, menu, Message::DismissMenu).into()
    }

    /// `_showTrackMenu`'s flags, for the take as the practice holds it now.
    fn track_menu(&self, track: &AudioTrack, practice: &SongPractice) -> TrackMenu {
        let vm = &*self.vm;
        let vocals = practice.active_vocal_tracks();
        let idx = vocals.iter().position(|t| t.id == track.id);
        TrackMenu {
            is_vocal: !track.is_backing(),
            can_merge_down: idx.is_some_and(|i| i + 1 < vocals.len()),
            // Only the Original (reference) can be separated — the
            // Accompaniment is already the backing. The item is HIDDEN when
            // the engine is unusable (the view model probes availability once
            // at construction, so it is resolved long before a menu opens) —
            // a dead action must not be offered.
            can_separate: track.is_reference() && vm.separation_available(),
            can_export: track.has_clip(),
            reseparate: vm.can_reseparate(),
            current_color_value: track.color_value,
        }
    }

    /// `AnalysisLanes` for a take folded out.
    fn lanes_view<'a>(
        &'a self,
        track: &'a AudioTrack,
        dur: i64,
        view_start: i64,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let id = track.id.as_str();
        let (metrics_loading, metrics, spectrogram_loading, spectrogram, lacks) =
            vm.with_analysis(|a, h| {
                (
                    a.metrics_loading_for(id, h),
                    a.metrics_for(id, h),
                    a.spectrogram_loading_for(id, h),
                    a.spectrogram_for(id, h),
                    a.lane_lacks_data(id, h),
                )
            });
        let image: &LaneImage = self.lane_images.get(id).unwrap_or(&self.no_image);
        let lanes = analysis_lanes(track, vm.analysis().lanes_for(id), dur, ms(vm.position()))
            .view_start_ms(view_start)
            .metrics(metrics_loading, metrics)
            .spectrogram(spectrogram_loading, spectrogram, image)
            .lacks_data(!vm.analysis().live_monitor() && lacks, vm.revision());
        let track_id = track.id.clone();
        Element::from(lanes).map(move |m| Message::Lanes {
            track_id: track_id.clone(),
            message: m,
        })
    }

    /// An empty-state drop hint: its own drop zone, and a click target that
    /// browses for its files.
    fn drop_hint_zone<'a>(
        &'a self,
        zone: DropZone,
        icon: Icon,
        hint: &'static str,
    ) -> Element<'a, Message> {
        let hover = self.drop_hover.as_ref() == Some(&zone);
        let tap = button(drop_hint(icon, hint, hover))
            .padding(0)
            .style(bare_button)
            .width(Length::Fill)
            .on_press(Message::Browse(zone.clone()));
        let padded = container(tap).padding(Padding {
            top: 4.0,
            right: 14.0,
            bottom: 10.0,
            left: 14.0,
        });
        drop_zone(padded.into(), zone)
    }

    /// `TakesHeader`: the "VOCAL TAKES" label plus "+ New take" and a
    /// dropdown that switches between the song's interpretations.
    fn takes_header<'a>(&'a self, practice: &'a SongPractice) -> Element<'a, Message> {
        let active = practice.active_version();
        let name = active.map_or("Interpretation", |v| v.name.as_str());
        let face = container(
            row![
                icons::icon(Icon::Drama, 13.0, Studio::ACCENT_BRIGHT),
                Space::new().width(7),
                // On a narrow centre pane the name gives way (ellipsized)
                // rather than pushing the chevron off the row.
                Reserve::new(
                    container(
                        studio_text(name)
                            .size(12)
                            .font(font::BOLD)
                            .color(Studio::TEXT_PRIMARY)
                            .ellipsis(1),
                    )
                    .max_width(170),
                    4.0 + 14.0,
                ),
                Space::new().width(4),
                icons::icon(Icon::ChevronDown, 14.0, Studio::TEXT_FAINT),
            ]
            .align_y(Vertical::Center),
        )
        .padding(Padding {
            top: 6.0,
            right: 9.0,
            bottom: 6.0,
            left: 11.0,
        })
        .style(theme::inset(7.0));
        // Opened at the press (`onTapDown`).
        let dropdown = mouse_area(face)
            .on_press(Message::OpenInterpretations)
            .interaction(mouse::Interaction::Pointer);
        let menu = (self.menu == Some(OpenMenu::Interpretations))
            .then(|| self.interpretations_menu(practice));
        let dropdown = menu_host(dropdown, menu, Message::DismissMenu);
        let new_take = button(
            container(
                row![
                    icons::icon(Icon::Plus, 13.0, Studio::TEXT_SECONDARY),
                    Space::new().width(5),
                    text("New take").size(11.5).color(Studio::TEXT_SECONDARY),
                ]
                .align_y(Vertical::Center),
            )
            .padding(Padding::from([5, 10]))
            .style(theme::card(6.0)),
        )
        .padding(0)
        .style(bare_button)
        .on_press(Message::NewTake);
        container(
            row![
                studio_text("VOCAL TAKES")
                    .size(11)
                    .font(font::BOLD)
                    .letter_spacing(0.8)
                    .color(Studio::TEXT_FAINT),
                Space::new().width(10),
                container(
                    row![new_take, Space::new().width(10), dropdown].align_y(Vertical::Center)
                )
                .width(Length::Fill)
                .align_x(Horizontal::Right),
            ]
            .align_y(Vertical::Center),
        )
        .padding(Padding {
            top: 16.0,
            right: 14.0,
            bottom: 10.0,
            left: 14.0,
        })
        .width(Length::Fill)
        .into()
    }

    /// The interpretation dropdown's menu: one row per interpretation, then
    /// new / rename / delete (delete only while there is more than one).
    fn interpretations_menu<'a>(&'a self, practice: &'a SongPractice) -> Element<'a, Message> {
        let active = practice.active_version().map(|v| v.id.as_str());
        let mut items: Vec<StudioMenuItem<InterpretationAction>> = practice
            .versions
            .iter()
            .map(|v| {
                StudioMenuItem::new(
                    v.name.clone(),
                    InterpretationAction::SetActive(v.id.clone()),
                )
                .selected(Some(v.id.as_str()) == active)
            })
            .collect();
        items.push(
            StudioMenuItem::new("New interpretation", InterpretationAction::Create)
                .divider(true)
                .icon(Icon::Plus),
        );
        items.push(StudioMenuItem::new("Rename", InterpretationAction::Rename).icon(Icon::Pencil));
        if practice.versions.len() > 1 {
            items.push(
                StudioMenuItem::new("Delete", InterpretationAction::Delete)
                    .icon(Icon::Trash2)
                    .danger(true),
            );
        }
        studio_menu(items, 224.0).map(Message::InterpretationMenu)
    }

    /// `_BusRow`: an FX bus lane — its header (name, mute, solo, fader; a
    /// tap selects the bus for the inspector, a right-click or ⋯ opens its
    /// menu), and its timeline area: the composite of every take routed
    /// into it.
    fn bus_row<'a>(
        &'a self,
        bus: &'a FxBus,
        practice: &'a SongPractice,
        dur: i64,
        view_start: i64,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let selected = vm.selected_bus_id() == Some(bus.id.as_str());
        let accent = bus.color_value.map_or(Studio::ACCENT, argb);
        let id = bus.id.clone();
        let fader_id = bus.id.clone();
        let content = container(column![
            row![
                studio_text(bus.name.as_str())
                    .size(11.5)
                    .font(font::BOLD)
                    .letter_spacing(0.2)
                    .color(accent)
                    .width(Length::Fill)
                    .ellipsis(1),
                track_menu_button(Message::OpenBusMenu(id.clone())),
            ]
            .align_y(Vertical::Center),
            Space::new().height(Length::Fill),
            row![
                ms_toggle(
                    "M",
                    bus.muted,
                    Studio::RECORD,
                    Message::ToggleBusMute(id.clone())
                ),
                Space::new().width(4),
                ms_toggle(
                    "S",
                    bus.soloed,
                    Studio::ACCENT,
                    Message::ToggleBusSolo(id.clone())
                ),
                Space::new().width(10),
                container(studio_fader(
                    bus.volume,
                    accent,
                    move |volume| Message::SetBusVolumeLive {
                        bus_id: fader_id.clone(),
                        volume,
                    },
                    Message::CommitFx,
                ))
                .width(Length::Fill),
                Space::new().width(8),
                fader_db_readout(bus.volume),
            ]
            .align_y(Vertical::Center),
        ])
        .padding(Padding {
            top: 8.0,
            right: 8.0,
            bottom: 8.0,
            left: 9.0,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        let header = container(row![
            container(Space::new())
                .width(3)
                .height(Length::Fill)
                .style(fill(accent)),
            content,
            container(Space::new())
                .width(1)
                .height(Length::Fill)
                .style(fill(Studio::BORDER)),
        ])
        .width(TRACK_HEADER_W)
        .height(Length::Fill)
        .style(fill(if selected {
            Studio::SURFACE_RAISED
        } else {
            Studio::PANEL
        }));
        let header = mouse_area(
            button(header)
                .padding(0)
                .style(bare_button)
                .on_press(Message::SelectBus(id.clone())),
        )
        .on_right_press(Message::OpenBusMenu(id.clone()))
        .interaction(mouse::Interaction::Pointer);
        let menu = (self.menu.as_ref() == Some(&OpenMenu::Bus(bus.id.clone()))).then(|| {
            bus_context_menu(&BusMenu {
                current_color_value: bus.color_value,
            })
            .map(Message::BusMenu)
        });
        let peaks = vm.with_mixer(|m, h| m.bus_waveform_peaks(h, &bus.id));
        let lane = envelope_lane(
            peaks,
            practice.timeline_duration_ms,
            view_start,
            dur,
            accent,
            ms(vm.position()),
        );
        let row = timeline_row(menu_host(header, menu, Message::DismissMenu).into(), lane);
        row
    }

    /// `_MasterRow`: the master bus lane — its header carries the overall
    /// output fader, and its timeline area shows a live composite of the
    /// audible mix (recomputed as tracks/levels change and while recording).
    fn master_row<'a>(
        &'a self,
        practice: &'a SongPractice,
        dur: i64,
        view_start: i64,
    ) -> Element<'a, Message> {
        let vm = &*self.vm;
        let volume = practice.master_volume;
        let content = container(column![
            studio_text("MASTER")
                .size(11.5)
                .font(font::BOLD)
                .letter_spacing(0.4)
                .color(Studio::ACCENT),
            Space::new().height(Length::Fill),
            row![
                container(studio_fader(
                    volume,
                    Studio::ACCENT,
                    Message::SetMasterVolumeLive,
                    Message::CommitMasterVolume,
                ))
                .width(Length::Fill),
                Space::new().width(8),
                fader_db_readout(volume),
            ]
            .align_y(Vertical::Center),
        ])
        .padding(Padding {
            top: 10.0,
            right: 8.0,
            bottom: 10.0,
            left: 9.0,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        let header = container(row![
            container(Space::new())
                .width(3)
                .height(Length::Fill)
                .style(fill(Studio::ACCENT)),
            content,
            container(Space::new())
                .width(1)
                .height(Length::Fill)
                .style(fill(Studio::BORDER)),
        ])
        .width(TRACK_HEADER_W)
        .height(Length::Fill)
        .style(fill(Studio::PANEL));
        let peaks = vm.with_mixer(|m, h| m.master_waveform_peaks(h));
        let lane = envelope_lane(
            peaks,
            practice.timeline_duration_ms,
            view_start,
            dur,
            Studio::ACCENT,
            ms(vm.position()),
        );
        timeline_row(header.into(), lane)
    }

    /// `_TimelineScrollbar`: a draggable horizontal scrollbar under the
    /// lanes — the reliable way to pan the timeline. Always shown: the thumb
    /// fills the groove at fit-all and narrows (and drags) once zoomed in.
    /// It sits in the timeline area, right of the header column.
    fn scrollbar<'a>(
        &'a self,
        practice: &'a SongPractice,
        span: i64,
        view_start: i64,
    ) -> Element<'a, Message> {
        if practice.timeline_duration_ms <= 0 {
            return Space::new().height(14).into();
        }
        // The view's own extent, which reaches past the timeline while a
        // take runs beyond its end: the practice's alone pinned the thumb at
        // the right end and panned at the wrong rate.
        let extent = self.vm.with_playback(|p, h| p.extent_ms(h));
        container(row![
            Space::new().width(TRACK_HEADER_W),
            canvas_widget(Scrollbar {
                start_ms: view_start,
                span_ms: span,
                extent_ms: extent,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        ])
        .height(14)
        .padding(Padding {
            left: TIMELINE_H_MARGIN,
            right: TIMELINE_H_MARGIN,
            ..Padding::ZERO
        })
        .into()
    }
}

// ------------------------------------------------------------ the pieces

/// An overlay's slot in the page's stack: the layer, or nothing in its place.
fn slot(layer: Option<Element<'_, Message>>) -> Element<'_, Message> {
    layer.unwrap_or_else(|| Space::new().into())
}

/// `_NoWorkspace`: `child` centred under a bar with the transport's back
/// control. Without it there was no way back at all while loading or when
/// the practice could not be loaded, and Home's open guard, released only
/// when this page pops, stayed set.
fn no_workspace(child: Element<'_, Message>) -> Element<'_, Message> {
    let back = button(
        container(icons::icon(Icon::ChevronLeft, 20.0, Studio::TEXT_SECONDARY))
            .center_x(30)
            .center_y(30),
    )
    .padding(0)
    .style(bare_button)
    .on_press(Message::Back);
    column![
        container(back)
            .width(Length::Fill)
            .height(53)
            .padding(Padding::from([0, 14]))
            .align_y(Vertical::Center)
            .style(fill(Studio::SURFACE)),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
        container(child)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(fill(Studio::BACKGROUND)),
    ]
    .into()
}

/// `_CatchUpGuard`: the workspace under the transport while the screen
/// catches up on a save an earlier screen on this practice left landing —
/// shown, dimmed, and taking no input: an edit made on the read from before
/// that save lands loses either itself or the save. The transport above
/// stays live, so Back and playback still work. One subtree either way, so
/// the workspace keeps its state when the guard lifts.
fn catch_up_guard(workspace: Element<'_, Message>, catching_up: bool) -> Element<'_, Message> {
    let mut layers = stack![container(workspace)
        .width(Length::Fill)
        .height(Length::Fill)];
    if catching_up {
        // A file dropped on the guard: nothing beneath takes it through the
        // guard, so it is said instead.
        let dim = container(text(FINISHING_SAVE).size(13).color(Studio::TEXT_SECONDARY))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(fill(Color::from_rgba8(
                0x0A,
                0x0A,
                0x0E,
                0x80 as f32 / 255.0,
            )));
        layers = layers.push(opaque(visible_drop_target(dim, |_| {
            Message::RefusedDrop(WAIT_FOR_SAVE)
        })));
    }
    container(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// How long the separation bar's fill takes to reach a new value.
pub const SEPARATION_FILL_ANIMATION: std::time::Duration = std::time::Duration::from_millis(280);

/// The separation bar, 8 tall: its fill moves to each new `progress` over
/// 280 ms, easing out (its `AnimatedContainer`).
fn separation_fill<'a>(progress: f64) -> Element<'a, Message> {
    animated(
        progress as f32,
        SEPARATION_FILL_ANIMATION,
        Curve::EaseOut,
        |value| {
            canvas_widget(ProgressBar { value })
                .width(Length::Fill)
                .height(8)
                .into()
        },
    )
    .into()
}

/// `_SeparationOverlay`: a blocking overlay while stems are separated (a
/// multi-minute background job). Nothing beneath takes a drop through it
/// (said instead), nor a key, nor a tap.
fn separation_overlay<'a>(progress: Option<f64>, can_cancel: bool) -> Element<'a, Message> {
    let p = progress.unwrap_or(0.0).clamp(0.0, 1.0);
    let pct = round_to_i64(p * 100.0);
    let badge = container(icons::icon(Icon::WandSparkles, 16.0, Studio::ACCENT))
        .center_x(32)
        .center_y(32)
        .style(|_| boxed(Some(alpha(Studio::ACCENT, 0.14)), 9.0, None));
    let card = container(column![
        row![
            badge,
            Space::new().width(12),
            container(
                text("Extracting the backing")
                    .size(15)
                    .font(font::BOLD)
                    .color(Studio::TEXT_PRIMARY)
            )
            .width(Length::Fill),
            studio_text(format!("{pct}%"))
                .size(14)
                .font(font::BOLD)
                .tabular()
                .color(Studio::ACCENT),
        ]
        .align_y(Vertical::Center),
        Space::new().height(18),
        // The fill animates; the percentage changes at once.
        separation_fill(p),
        Space::new().height(16),
        text("Pulling the instrumental out of the song to sing over.\nThis can take a minute or two.")
            .size(12)
            .line_height(1.45)
            .color(Studio::TEXT_FAINT),
        Space::new().height(20),
        container(studio_button("Cancel", Message::CancelSeparation).enabled(can_cancel))
            .width(Length::Fill)
            .align_x(Horizontal::Right),
    ])
    .width(380)
    // 24 / 22 / 24 / 20 inside the 1 px border.
    .padding(Padding {
        top: 23.0,
        right: 25.0,
        bottom: 21.0,
        left: 25.0,
    })
    .style(|_| container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0x80 as f32 / 255.0),
            offset: Vector::new(0.0, 16.0),
            blur_radius: 40.0,
        },
        ..boxed(Some(Studio::SURFACE_RAISED), 16.0, Some(Studio::BORDER))
    });
    let barrier = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(fill(Color::from_rgba8(
            0x0A,
            0x0A,
            0x0E,
            0xC4 as f32 / 255.0,
        )));
    holds_keyboard(opaque(visible_drop_target(barrier, |_| {
        Message::RefusedDrop(WAIT_FOR_SEPARATION)
    })))
}

/// The toast: a floating snack bar 16 in from the page's edges at its
/// bottom, `Studio.surfaceRaised`, radius 8, `Studio.border`, 13 px text, at
/// `opacity`. It takes its own taps. (The app draws it too, once the page
/// has handed it over.)
pub(super) fn snack_bar(message: &str, opacity: f32) -> Element<'_, Infallible> {
    let fade = move |c: Color| alpha(c, c.a * opacity);
    let bar = container(text(message).size(13).color(fade(Studio::TEXT_PRIMARY)))
        .width(Length::Fill)
        .padding(Padding::from([14, 16]))
        .style(move |_: &Theme| container::Style {
            shadow: Shadow {
                color: alpha(Color::BLACK, 0.4 * opacity),
                offset: Vector::new(0.0, 3.0),
                blur_radius: 8.0,
            },
            ..boxed(
                Some(fade(Studio::SURFACE_RAISED)),
                8.0,
                Some(fade(Studio::BORDER)),
            )
        });
    container(opaque(bar))
        .width(Length::Fill)
        .padding(16)
        .into()
}

/// A section label of the lane list ("MASTER TRACK", "FX BUSES"' text): 11
/// px w700, 0.8 letter spacing.
fn section_label<'a>(label: &'static str, top: f32) -> Element<'a, Message> {
    container(
        studio_text(label)
            .size(11)
            .font(font::BOLD)
            .letter_spacing(0.8)
            .color(Studio::TEXT_FAINT),
    )
    .padding(Padding {
        top,
        right: 14.0,
        bottom: 10.0,
        left: 14.0,
    })
    .width(Length::Fill)
    .into()
}

/// `_BackingHeader`: the section label plus the segmented Accompaniment ⇄
/// Original toggle that one-click switches which backing plays (and is
/// shown). Switching is seamless — the playhead doesn't move.
fn backing_header<'a>(reference: bool) -> Element<'a, Message> {
    let seg = |label: &'static str, active: bool, value: bool| {
        button(
            container(
                studio_text(label)
                    .size(11)
                    .font(font::BOLD)
                    .letter_spacing(0.3)
                    .color(if active {
                        Studio::ACCENT_BRIGHT
                    } else {
                        Studio::TEXT_SECONDARY
                    }),
            )
            .padding(Padding::from([4, 12]))
            .style(move |_| {
                boxed(
                    Some(if active {
                        alpha(Studio::ACCENT, 0.18)
                    } else {
                        Color::TRANSPARENT
                    }),
                    5.0,
                    None,
                )
            }),
        )
        .padding(0)
        .style(bare_button)
        .on_press(Message::SetPlayReference(value))
    };
    let toggle = container(row![
        seg("Accompaniment", !reference, false),
        seg("Original", reference, true),
    ])
    // 2 inside the 1 px border.
    .padding(3)
    .style(theme::inset(7.0));
    container(
        row![
            studio_text(if reference {
                "REFERENCE · ORIGINAL"
            } else {
                "ACCOMPANIMENT"
            })
            .size(11)
            .font(font::BOLD)
            .letter_spacing(0.8)
            .color(Studio::TEXT_FAINT),
            Space::new().width(Length::Fill),
            toggle,
        ]
        .align_y(Vertical::Center),
    )
    .padding(Padding {
        top: 12.0,
        right: 14.0,
        bottom: 8.0,
        left: 14.0,
    })
    .width(Length::Fill)
    .into()
}

/// `_BusesHeader`: the "FX BUSES" label and "+ New bus".
fn buses_header<'a>() -> Element<'a, Message> {
    let new_bus = button(
        container(
            row![
                icons::icon(Icon::Plus, 13.0, Studio::TEXT_SECONDARY),
                Space::new().width(5),
                text("New bus").size(11.5).color(Studio::TEXT_SECONDARY),
            ]
            .align_y(Vertical::Center),
        )
        .padding(Padding::from([5, 10]))
        .style(theme::card(6.0)),
    )
    .padding(0)
    .style(bare_button)
    .on_press(Message::NewBus);
    container(
        row![
            studio_text("FX BUSES")
                .size(11)
                .font(font::BOLD)
                .letter_spacing(0.8)
                .color(Studio::TEXT_FAINT),
            Space::new().width(Length::Fill),
            new_bus,
        ]
        .align_y(Vertical::Center),
    )
    .padding(Padding {
        top: 18.0,
        right: 14.0,
        bottom: 10.0,
        left: 14.0,
    })
    .width(Length::Fill)
    .into()
}

/// How long a drop hint's box takes to light up or go back.
pub const DROP_HINT_ANIMATION: std::time::Duration = std::time::Duration::from_millis(120);

/// `_DropHint`: an icon and a line, lit up (accent border + tint) while a
/// file is dragged over the area, so the drop target is the box itself —
/// not the whole background. The box's tint and border change over 120 ms,
/// linear (its `AnimatedContainer`); the icon and line at once.
fn drop_hint<'a>(icon: Icon, hint: &'static str, highlighted: bool) -> Element<'a, Message> {
    let color = if highlighted {
        Studio::ACCENT
    } else {
        Studio::TEXT_FAINT
    };
    animated_container(
        row![
            icons::icon(icon, 16.0, color),
            Space::new().width(8),
            // Cut short rather than overflowing: at the smallest window with
            // both side panels open the centre pane is under 400 px wide.
            studio_text(hint).size(13).color(color).ellipsis(1),
        ]
        .align_y(Vertical::Center),
        DROP_HINT_ANIMATION,
    )
    .width(Length::Fill)
    .height(64)
    .align((0.0, 0.0))
    .decoration(if highlighted {
        Decoration::boxed(alpha(Studio::ACCENT, 0.08), 8.0, Studio::ACCENT, 1.5)
    } else {
        Decoration::boxed(Studio::PANEL, 8.0, Studio::BORDER, 1.0)
    })
    .into()
}

/// `_AudioDropZone`: a file drop target with its own hover state, taking a
/// drop only where it can be seen (a drop over a dialog, the lyrics Focus
/// view, or the dock over a lane scrolled out of view imported into that
/// hidden take).
fn drop_zone(content: Element<'_, Message>, zone: DropZone) -> Element<'_, Message> {
    let hover_zone = zone.clone();
    visible_drop_target(content, move |paths| Message::Drop {
        zone: zone.clone(),
        paths,
    })
    .on_hover(move |hover| Message::DropHover {
        zone: hover_zone.clone(),
        hover,
    })
    .into()
}

/// A 74 px timeline row inset to the lanes: `header` over the header
/// column, `lane` across the timeline, a 1 px line under both.
fn timeline_row<'a>(
    header: Element<'a, Message>,
    lane: Element<'a, Message>,
) -> Element<'a, Message> {
    container(column![
        row![header, lane].height(Length::Fill),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
    ])
    .width(Length::Fill)
    .height(TRACK_ROW_HEIGHT)
    .padding(Padding {
        left: TIMELINE_H_MARGIN,
        right: TIMELINE_H_MARGIN,
        ..Padding::ZERO
    })
    .into()
}

/// The timeline area of the master or a bus lane: the grid, the envelope
/// (inset 10 px top and bottom) and the playhead, on `Studio.inset`.
fn envelope_lane<'a>(
    peaks: Arc<[f64]>,
    extent_ms: i64,
    view_start: i64,
    span: i64,
    color: Color,
    position_ms: i64,
) -> Element<'a, Message> {
    container(stack![
        canvas_widget(TimelineGridPainter {
            timeline_duration_ms: span,
            color: Studio::GRID,
            start_ms: view_start,
        })
        .width(Length::Fill)
        .height(Length::Fill),
        container(
            timeline_envelope_painter(peaks, extent_ms, view_start, span, color).into_canvas()
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::from([10, 0])),
        canvas_widget(PlayheadPainter {
            position_ms,
            timeline_duration_ms: span,
            color: Studio::PLAYHEAD,
            show_head: false,
            start_ms: view_start,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(fill(Studio::INSET))
    .clip(true)
    .into()
}

// ------------------------------------------------------------- panning

/// The pan wrapper's memory: whether Shift is held.
#[derive(Debug, Default)]
struct PanState {
    shift: bool,
}

/// The pan for a scroll of `(x, y)` px (iced's sign: positive scrolls the
/// content right/down) with Shift `shift`, over a timeline `width` px wide
/// showing `span` ms: Flutter's scroll delta (the negation), `dx` — or `dy`
/// with Shift held — mapped to the view. `None` when there is nothing to
/// pan.
pub(super) fn wheel_pan_ms(x: f32, y: f32, shift: bool, width: f32, span: i64) -> Option<i64> {
    if width <= 0.0 {
        return None;
    }
    let delta = if shift { -y } else { -x };
    if delta == 0.0 {
        return None;
    }
    Some(round_to_i64(
        f64::from(delta) / f64::from(width) * span as f64,
    ))
}

/// The `Listener` over the lane list: pans the viewport on a two-finger
/// horizontal swipe (iced delivers it as a wheel scroll's x) or Shift+wheel;
/// plain vertical scroll falls through to the list.
fn pan_area(content: Element<'_, Message>, span: i64) -> Element<'_, Message> {
    Wrap::<Message, PanState>::new(
        content,
        Box::new(move |state, event, bounds, cursor, shell| match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.shift = modifiers.shift();
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if cursor.position_over(bounds).is_none() {
                    return;
                }
                let (x, y) = match *delta {
                    mouse::ScrollDelta::Lines { x, y } => (x * LINE_PX, y * LINE_PX),
                    mouse::ScrollDelta::Pixels { x, y } => (x, y),
                };
                // The timeline area starts after the fixed track-header
                // column and the insets.
                let width = bounds.width - TRACK_HEADER_W - 2.0 * TIMELINE_H_MARGIN;
                if let Some(ms) = wheel_pan_ms(x, y, state.shift, width, span) {
                    shell.publish(Message::PanView(ms));
                }
            }
            _ => {}
        }),
    )
    .into()
}

// ------------------------------------------------------------ painters

/// The loop band across the timeline area (`Positioned` +
/// `IgnorePointer(ColoredBox)`), clamped to the timeline viewport: a loop
/// that starts before the visible window must not paint over the
/// track-header column, nor overshoot past the right edge.
struct LoopBand {
    start_ms: i64,
    end_ms: i64,
    view_start_ms: i64,
    span_ms: i64,
}

impl<M> canvas::Program<M> for LoopBand {
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
        let w = f64::from(bounds.width - TRACK_HEADER_W - 2.0 * TIMELINE_H_MARGIN);
        if w > 0.0 && self.span_ms > 0 {
            let x = |at: i64| {
                ((at - self.view_start_ms) as f64 / self.span_ms as f64 * w).clamp(0.0, w)
            };
            let (left, right) = (x(self.start_ms), x(self.end_ms));
            if right > left {
                frame.fill_rectangle(
                    Point::new(
                        TIMELINE_H_MARGIN + TRACK_HEADER_W + left as f32,
                        RULER_HEIGHT,
                    ),
                    Size::new((right - left) as f32, bounds.height - RULER_HEIGHT),
                    alpha(Studio::ACCENT, 0.06),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The scrollbar's groove inset (horizontal, vertical).
const GROOVE_INSET: (f32, f32) = (4.0, 3.0);

/// How far a mouse moves horizontally before the thumb's drag wins
/// (`kPrecisePointerHitSlop`).
const THUMB_DRAG_SLOP: f32 = 1.0;

/// The timeline scrollbar's groove and thumb, and the thumb's drag.
struct Scrollbar {
    start_ms: i64,
    span_ms: i64,
    extent_ms: i64,
}

/// A press on the thumb in flight.
#[derive(Debug, Default)]
struct ThumbDrag {
    press: Option<(f32, f32, bool)>,
}

impl Scrollbar {
    /// The thumb's box in `bounds`' own coordinates.
    fn thumb(&self, bounds: Rectangle) -> (Rectangle, f64) {
        let groove = Rectangle::new(
            Point::new(GROOVE_INSET.0, GROOVE_INSET.1),
            Size::new(
                (bounds.width - 2.0 * GROOVE_INSET.0).max(0.0),
                (bounds.height - 2.0 * GROOVE_INSET.1).max(0.0),
            ),
        );
        let t = timeline_scroll_thumb(
            self.start_ms,
            self.span_ms,
            self.extent_ms,
            f64::from(groove.width),
        );
        (
            Rectangle::new(
                Point::new(groove.x + t.left as f32, groove.y),
                Size::new(t.width as f32, groove.height),
            ),
            t.ms_per_px,
        )
    }
}

impl canvas::Program<Message> for Scrollbar {
    type State = ThumbDrag;

    fn update(
        &self,
        state: &mut ThumbDrag,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let (thumb, _) = self.thumb(bounds);
                if !thumb.contains(p) {
                    return None;
                }
                state.press = Some((p.x, p.x, false));
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let (down, last, dragging) = state.press.as_mut()?;
                let x = position.x - bounds.x;
                if !*dragging {
                    // The movement up to the slop is swallowed
                    // (`DragStartBehavior.start`).
                    if (x - *down).abs() > THUMB_DRAG_SLOP {
                        *dragging = true;
                        *last = x;
                    }
                    return None;
                }
                let dx = x - *last;
                *last = x;
                let (_, ms_per_px) = self.thumb(bounds);
                if ms_per_px <= 0.0 || dx == 0.0 {
                    return None;
                }
                Some(canvas::Action::publish(Message::PanView(round_to_i64(
                    f64::from(dx) * ms_per_px,
                ))))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.press.take()?;
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &ThumbDrag,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let groove_size = Size::new(
            (bounds.width - 2.0 * GROOVE_INSET.0).max(0.0),
            (bounds.height - 2.0 * GROOVE_INSET.1).max(0.0),
        );
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(GROOVE_INSET.0, GROOVE_INSET.1),
                groove_size,
                4.0.into(),
            ),
            Studio::INSET,
        );
        let (thumb, _) = self.thumb(bounds);
        frame.fill(
            &Path::rounded_rectangle(thumb.position(), thumb.size(), 4.0.into()),
            Studio::BORDER_STRONG,
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &ThumbDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let (thumb, _) = self.thumb(bounds);
        if state.press.is_some()
            || cursor
                .position_in(bounds)
                .is_some_and(|p| thumb.contains(p))
        {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::None
        }
    }
}

/// The separation's progress bar: `Studio.inset`, the done part an accent
/// gradient, radius 5.
struct ProgressBar {
    value: f32,
}

impl<M> canvas::Program<M> for ProgressBar {
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
        frame.fill(
            &Path::rounded_rectangle(Point::ORIGIN, bounds.size(), 5.0.into()),
            Studio::INSET,
        );
        let w = self.value * bounds.width;
        if w > 0.0 {
            let shade = gradient::Linear::new(Point::ORIGIN, Point::new(w, 0.0))
                .add_stop(0.0, Studio::ACCENT)
                .add_stop(1.0, Studio::ACCENT_BRIGHT);
            frame.fill(
                &Path::rounded_rectangle(
                    Point::ORIGIN,
                    Size::new(w, bounds.height),
                    5.0f32.min(w / 2.0).into(),
                ),
                shade,
            );
        }
        vec![frame.into_geometry()]
    }
}

// --------------------------------------------------------------- Reserve

/// Lays `content` out with `reserve` px less than it is offered, so what
/// follows it in a row keeps that room: Flutter's `Flexible` in a row of
/// fixed siblings (iced gives a row's sized children their room in order,
/// so a long name would have taken the chevron's).
struct Reserve<'a> {
    content: Element<'a, Message>,
    reserve: f32,
}

impl<'a> Reserve<'a> {
    fn new(content: impl Into<Element<'a, Message>>, reserve: f32) -> Self {
        Self {
            content: content.into(),
            reserve,
        }
    }
}

impl Widget<Message, Theme, Renderer> for Reserve<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let max = limits.max();
        let limits = layout::Limits::new(
            limits.min(),
            Size::new((max.width - self.reserve).max(0.0), max.height),
        );
        self.content.as_widget_mut().layout(tree, renderer, &limits)
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a> From<Reserve<'a>> for Element<'a, Message> {
    fn from(r: Reserve<'a>) -> Self {
        Element::new(r)
    }
}

#[cfg(test)]
mod animations {
    use iced::time::Instant;

    use super::*;
    use crate::ui::core::motion::Lerp;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    #[test]
    fn a_drop_hint_lights_up_over_120_ms() {
        let view = |lit| drop_hint(Icon::Music, "Drop a song", lit);
        let mut window = Frames::new(Size::new(300.0, 64.0));
        // Inside the border, clear of the icon and the line.
        let fill = Point::new(5.0, 32.0);
        let (rest, glow) = (Studio::PANEL, alpha(Studio::ACCENT, 0.08));
        let shows = |w: &Frames, c: Color| near(w.pixel(fill), over(c, BACKGROUND));
        let t0 = Instant::now();
        window.frame(view(false), &[], t0);
        assert!(shows(&window, rest));
        assert!(window.frame(view(true), &[], t0));
        assert!(shows(&window, rest), "start");
        window.frame(view(true), &[], t0 + 60 * MS);
        assert!(shows(&window, rest.lerp(glow, 0.5)), "middle");
        assert!(!window.frame(view(true), &[], t0 + 120 * MS));
        assert!(shows(&window, glow), "end");
    }

    #[test]
    fn the_separation_bar_eases_its_fill_out_over_280_ms() {
        let mut window = Frames::new(Size::new(200.0, 8.0));
        let filled = |w: &Frames, x: f32| !near(w.pixel(Point::new(x, 4.0)), Studio::INSET);
        let t0 = Instant::now();
        window.frame(separation_fill(0.0), &[], t0);
        assert!(!filled(&window, 4.0), "empty");
        assert!(window.frame(separation_fill(1.0), &[], t0));
        assert!(!filled(&window, 100.0), "start");
        window.frame(separation_fill(1.0), &[], t0 + 140 * MS);
        // Eased out, the fill is past halfway (68 %; linear would be 50 %).
        assert!(filled(&window, 130.0) && !filled(&window, 145.0), "middle");
        assert!(!window.frame(separation_fill(1.0), &[], t0 + 280 * MS));
        assert!(filled(&window, 195.0), "end");
    }
}
