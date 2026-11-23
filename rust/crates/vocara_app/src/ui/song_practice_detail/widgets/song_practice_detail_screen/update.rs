//! The page's update: each message routed to the view-model call the Dart
//! widget made, the flows the Dart screen awaited (the drops, the note and
//! technique editors, the confirmations and name prompts, the menus), and
//! what every rebuild did after the frame.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use iced::Task;
use vocara_model::util::format::format_clock_ms;

use super::{
    backing_set_toast, is_audio_path, ms, panel_data, Asked, Awaited, DropRun, DropZone,
    InterpretationAction, Message, NoteTarget, OpenMenu, OpenPicker, Owed, Render, RunKind,
    SongPracticeDetailScreen, Source, Step, NO_AUDIO_IN_DROP, SNACK_DURATION,
};
use crate::router::{Navigation, Route};
use crate::ui::core::widgets::confirm_dialog::{ConfirmDialog, Dialog, DialogResult};
use crate::ui::song_practice_detail::view_model::song_practice_detail_view_model::{
    normalize_marker_range, DockTab, Effect, LyricsMode, Reply, RightTab, Started,
};
use crate::ui::song_practice_detail::widgets::analysis_lanes;
use crate::ui::song_practice_detail::widgets::bottom_dock::{self, resized_height};
use crate::ui::song_practice_detail::widgets::detail_transport_bar as transport;
use crate::ui::song_practice_detail::widgets::fx_panel;
use crate::ui::song_practice_detail::widgets::left_panel::{self, Inspector};
use crate::ui::song_practice_detail::widgets::lyrics_panel;
use crate::ui::song_practice_detail::widgets::mixer_strip;
use crate::ui::song_practice_detail::widgets::note_editor::{self, NoteEditor};
use crate::ui::song_practice_detail::widgets::ruler_bar;
use crate::ui::song_practice_detail::widgets::technique_picker::{self, TechniquePicker};
use crate::ui::song_practice_detail::widgets::track_context_menu::{
    confirm_delete_bus, rename_dialog, BusAction, ClipAction, TrackAction,
};
use crate::ui::song_practice_detail::widgets::track_row;
use crate::ui::song_practice_detail::widgets::voiceprint_view as voiceprint;
use crate::ui::Update;

impl SongPracticeDetailScreen {
    /// Handles one message; what it asked of the runtime.
    pub(super) fn handle(&mut self, message: Message) -> Step {
        let mut step = Step::new();
        if self.disposed {
            // Gone from the stack: only the answers to work the view model
            // still has in flight, so it lands.
            self.answer(message, &mut step);
            return step;
        }
        match message {
            // `popOrGo(context, Routes.home)`: no workspace, no take.
            Message::Back => step.update.navigation = Some(Navigation::PopOrGo(Route::Home)),
            Message::Transport(m) => self.transport(m, &mut step),
            Message::Ruler(m) => match m {
                ruler_bar::Message::Seek(to) => self.vm.seek(to),
                ruler_bar::Message::SetLoopStart(at) => {
                    let fx = self.vm.playback_do(|p, h| p.set_loop_start(h, at));
                    self.absorb(fx, &mut step);
                }
                ruler_bar::Message::SetLoopEnd(at) => {
                    let fx = self.vm.playback_do(|p, h| p.set_loop_end(h, at));
                    self.absorb(fx, &mut step);
                }
            },
            Message::Row { track_id, message } => self.row_message(track_id, message, &mut step),
            Message::Lanes { track_id, message } => self.lanes(track_id, message, &mut step),
            Message::SetPlayReference(value) => {
                let fx = self.vm.set_play_reference(value);
                self.absorb(fx, &mut step);
            }
            Message::NewTake => {
                let fx = self.vm.new_take();
                self.absorb(fx, &mut step);
            }
            Message::OpenInterpretations => self.menu = Some(OpenMenu::Interpretations),
            Message::NewBus => {
                let fx = self.vm.mixer_do(|m, h| m.create_bus(h, ""));
                self.absorb(fx, &mut step);
            }
            Message::SelectBus(id) => self.vm.select_bus(Some(&id)),
            Message::OpenBusMenu(id) => self.menu = Some(OpenMenu::Bus(id)),
            Message::ToggleBusMute(id) => {
                let fx = self.vm.mixer_do(|m, h| m.toggle_bus_mute(h, &id));
                self.absorb(fx, &mut step);
            }
            Message::ToggleBusSolo(id) => {
                let fx = self.vm.mixer_do(|m, h| m.toggle_bus_solo(h, &id));
                self.absorb(fx, &mut step);
            }
            Message::SetBusVolumeLive { bus_id, volume } => {
                self.owe(Owed::Fx(Source::Lane(Some(bus_id.clone()))));
                let fx = self
                    .vm
                    .mixer_do(|m, h| m.set_bus_volume_live(h, &bus_id, volume));
                self.absorb(fx, &mut step);
            }
            Message::CommitFx => {
                self.owed
                    .retain(|o| !matches!(o, Owed::Fx(Source::Lane(_))));
                let fx = self.vm.commit_fx();
                self.absorb(fx, &mut step);
            }
            Message::SetMasterVolumeLive(gain) => {
                self.owe(Owed::MasterVolume(Source::Lane(None)));
                let fx = self.vm.mixer_do(|m, h| m.set_master_volume_live(h, gain));
                self.absorb(fx, &mut step);
            }
            Message::CommitMasterVolume => {
                self.owed
                    .retain(|o| *o != Owed::MasterVolume(Source::Lane(None)));
                let fx = self.vm.mixer_do(|m, _| m.commit_master_volume());
                self.absorb(fx, &mut step);
            }
            Message::PanView(delta) => {
                let fx = self.vm.playback_do(|p, h| p.pan_view(h, delta));
                self.absorb(fx, &mut step);
            }
            Message::Drop { zone, paths } => {
                self.drop_hover = None;
                self.drop_files(zone, paths, None, &mut step);
            }
            Message::DropHover { zone, hover } => {
                if hover {
                    self.drop_hover = Some(zone);
                } else if self.drop_hover.as_ref() == Some(&zone) {
                    self.drop_hover = None;
                }
            }
            Message::Browse(zone) => {
                // Refused before the open panel, not after a file was picked
                // in it (as the lane menu's Add clip does).
                if !self.refused(&mut step) {
                    step.picks.push((zone, None));
                }
            }
            Message::Picked { zone, paths, at_ms } => {
                if !paths.is_empty() {
                    self.drop_files(zone, paths, at_ms, &mut step);
                }
            }
            Message::RefusedDrop(text) => self.toast(text.to_string(), &mut step),
            Message::TrackMenu(action) => self.track_action(action, &mut step),
            Message::ClipMenu(action) => self.clip_action(action, &mut step),
            Message::BusMenu(action) => self.bus_action(action, &mut step),
            Message::InterpretationMenu(action) => self.interpretation_action(action, &mut step),
            Message::DismissMenu => self.menu = None,
            Message::Inspector(m) => self.inspector(m, Source::Inspector, &mut step),
            Message::Fx(m) => self.fx(m, &mut step),
            Message::Lyrics(m) => self.lyrics_message(m, &mut step),
            Message::Dock(m) => self.dock(m, &mut step),
            Message::NoteEditor(m) => self.note_editor_message(m, &mut step),
            Message::TechniquePicker(m) => self.picker_message(m, &mut step),
            Message::Answered(asked, result) => self.answered(asked, result, &mut step),
            Message::CancelSeparation => self.vm.cancel_separation(),
            Message::Frame | Message::EngineEvent => {}
            Message::PumpCapture => self.vm.pump_capture(),
            Message::MicTick => {
                let fx = self.vm.mic_tick();
                self.absorb(fx, &mut step);
            }
            Message::MonitorMeterTick => self.vm.monitor_meter_tick(),
            Message::DeviceWatchTick => {
                let fx = self.vm.device_watch_tick();
                self.absorb(fx, &mut step);
            }
            Message::SeparationProgressTick => self.vm.separation_progress_tick(),
            Message::QuitRequests => {
                let fx = self.vm.serve_quit_requests();
                self.absorb(fx, &mut step);
            }
            Message::RemoteApplied(event) => {
                let fx = self.vm.on_remote_applied(&event);
                self.absorb(fx, &mut step);
            }
            Message::SnackExpired(seq) => {
                if self.snack.as_ref().is_some_and(|(s, _)| *s == seq) {
                    self.snack = None;
                }
            }
            Message::Done => {}
            answer => self.answer(answer, &mut step),
        }
        self.refresh(&mut step);
        step
    }

    /// The answers to the view model's work.
    fn answer(&mut self, message: Message, step: &mut Step) {
        let fx = match message {
            Message::Job(landed) => match landed.take() {
                Some(done) => self.vm.job_done(done),
                None => return,
            },
            Message::AnalysisJob(done) => self.vm.analysis_job_done(done),
            Message::AnalysisMicPermission(granted) => {
                self.vm.analysis_mic_permission_answered(granted)
            }
            Message::LaneHealRetry => {
                self.vm.lane_heal_retry_due();
                return;
            }
            Message::TakeSetup(request) => self.vm.take_setup(request),
            Message::StartDone(done) => self.vm.start_done(done),
            Message::RecordingJob(done) => self.vm.recording_job_done(done),
            Message::Finalized(op, outcome) => self.vm.finalize_ran(op, outcome),
            Message::FinalizeRetry(op) => self.vm.finalize_retry_due(op),
            Message::MonitorPermission(granted) => self.vm.monitor_permission_answered(granted),
            Message::CleanedUp(op, outcome) => self.vm.interpretations_cleaned_up(op, outcome),
            _ => return,
        };
        self.absorb(fx, step);
    }

    /// Takes in what a view model call asked: its work to run, its toasts,
    /// its leave, and its replies (the Dart screen's awaits resuming).
    pub(super) fn absorb(&mut self, effects: Vec<Effect>, step: &mut Step) {
        for effect in effects {
            match effect {
                Effect::Toast(text) => self.toast(text, step),
                Effect::Leave => {
                    if !self.disposed {
                        step.update.navigation = Some(Navigation::PopOrGo(Route::Home));
                    }
                }
                Effect::Reply(reply) => self.reply(reply, step),
                work => step.work.push(work),
            }
        }
    }

    /// Waits for `started`'s reply, which resumes `awaited`.
    fn await_call(&mut self, started: Started, awaited: Awaited, step: &mut Step) {
        self.awaiting.insert(started.call, awaited);
        self.absorb(started.effects, step);
    }

    fn reply(&mut self, reply: Reply, step: &mut Step) {
        let call = match &reply {
            Reply::Done(call)
            | Reply::ClipAdded { call, .. }
            | Reply::LyricsSaved { call, .. }
            | Reply::Techniques { call, .. } => *call,
        };
        let Some(awaited) = self.awaiting.remove(&call) else {
            return;
        };
        if self.disposed {
            return;
        }
        match (awaited, reply) {
            (Awaited::Drop(run), reply) => {
                if let Reply::ClipAdded {
                    end_ms: Some(end), ..
                } = reply
                {
                    if let Some(RunKind::Clip { at_ms, .. }) = self
                        .runs
                        .iter_mut()
                        .find(|r| r.id == run)
                        .map(|r| &mut r.kind)
                    {
                        *at_ms = end;
                    }
                }
                self.continue_run(run, step);
            }
            (Awaited::Techniques(seq), Reply::Techniques { techniques, .. }) => {
                // A list for a picker that is gone has nowhere to land.
                if let Some(open) = self.picker.as_mut().filter(|p| p.seq == seq) {
                    open.picker.loaded(techniques);
                }
            }
            (Awaited::LyricsImport(ticket), _) => {
                let replaced = {
                    let vm = &*self.vm;
                    let as_text = || vm.lyrics_as_text();
                    let data = panel_data(vm, &self.shown, &as_text, false);
                    self.lyrics.end_import(ticket, &data)
                };
                // The editor is made again from the imported lyrics (or it
                // would reopen on the draft).
                if replaced {
                    self.vm.set_lyrics_draft(None);
                }
            }
            (Awaited::LyricsSave(ticket), Reply::LyricsSaved { landed, .. }) => {
                let leave = {
                    let vm = &*self.vm;
                    let as_text = || vm.lyrics_as_text();
                    let data = panel_data(vm, &self.shown, &as_text, false);
                    self.lyrics.save_landed(&ticket, landed, &data)
                };
                if leave {
                    self.vm.set_lyrics_mode(LyricsMode::Read);
                }
            }
            _ => {}
        }
    }

    /// `_showToast`: the snack bar, replacing the one showing
    /// (`clearSnackBars`). Not once the page is gone.
    pub(super) fn toast(&mut self, text: String, step: &mut Step) {
        if self.disposed {
            return;
        }
        self.snack_seq += 1;
        let seq = self.snack_seq;
        self.snack = Some((seq, text));
        step.task(Task::perform(
            async move { tokio::time::sleep(SNACK_DURATION).await },
            move |_| Message::SnackExpired(seq),
        ));
    }

    /// `vm.refuseStructuralEdit()`: whether an import or a structural edit
    /// is refused now (said, in the words every refused edit uses).
    fn refused(&mut self, step: &mut Step) -> bool {
        let (refused, fx) = self.vm.refuse_structural_edit();
        self.absorb(fx, step);
        refused
    }

    /// A studio dialog over the window; its answer comes back as
    /// [`Message::Answered`].
    fn ask(&mut self, dialog: impl Into<Dialog>, asked: Asked, step: &mut Step) {
        self.dialogs_open += 1;
        step.update.dialog =
            Update::dialog(dialog, move |result| Message::Answered(asked, result)).dialog;
    }

    fn owe(&mut self, owed: Owed) {
        if !self.owed.contains(&owed) {
            self.owed.push(owed);
        }
    }

    // ------------------------------------------------------------ routes

    fn transport(&mut self, m: transport::Message, step: &mut Step) {
        let fx = match m {
            transport::Message::Back => return self.back(step),
            transport::Message::TogglePlay => self.vm.playback_do(|p, h| p.toggle_play(h)),
            transport::Message::Stop => self.vm.playback_do(|p, h| p.stop(h)),
            transport::Message::StopRecording => self.vm.stop_recording(),
            transport::Message::ToggleLoop => self.vm.playback_do(|p, h| p.toggle_loop(h)),
            transport::Message::ToggleFollowCursor => {
                self.vm.playback_do(|p, h| p.toggle_follow_cursor(h))
            }
            transport::Message::ToggleInputMonitor => {
                self.vm.recording_do(|r, h| r.toggle_input_monitor(h))
            }
            transport::Message::StartRecording => self.vm.recording_do(|r, h| r.start_recording(h)),
            transport::Message::ZoomTimeline(factor) => {
                self.vm.playback_do(|p, h| p.zoom_timeline(h, factor))
            }
            transport::Message::SetTimelineZoomLive(value) => self
                .vm
                .playback_do(|p, h| p.set_timeline_zoom_live(h, value)),
            transport::Message::CommitTimelineZoom => {
                self.vm.playback_do(|p, _| p.commit_timeline_zoom())
            }
        };
        self.absorb(fx, step);
    }

    /// The transport's back chevron (`onBack`). A programmatic pop is not
    /// vetoed by the leave guard, so an in-flight take is finalized here
    /// first — and in REC's arm window the start is cancelled first, or it
    /// could still arm on the leaving page: the page leaves on the view
    /// model's [`Effect::Leave`]. Pop, never go, when Home pushed this page:
    /// Home waits for that push to reload its list and release its open
    /// guard. Go is only the fallback for a deep link, where nothing lies
    /// beneath to pop to.
    fn back(&mut self, step: &mut Step) {
        if self.vm.take_holds_screen() {
            let fx = self.vm.stop_before_leaving();
            self.absorb(fx, step);
            return;
        }
        step.update.navigation = Some(Navigation::PopOrGo(Route::Home));
    }

    fn row_message(&mut self, track_id: String, m: track_row::Message, step: &mut Step) {
        let id = track_id.as_str();
        let fx = match m {
            track_row::Message::Select => self.vm.select_track(id),
            track_row::Message::SelectClip(clip) => self.vm.select_clip(id, &clip),
            track_row::Message::ClipMoveLive { clip_id, start_ms } => {
                self.moving = Some(track_id.clone());
                self.vm.move_clip_start_live(id, &clip_id, start_ms);
                return;
            }
            track_row::Message::MoveCommit => {
                self.moving = None;
                self.vm.commit_track_move()
            }
            track_row::Message::ClipContextMenu(clip_id) => {
                self.menu = Some(OpenMenu::Clip { track_id, clip_id });
                return;
            }
            track_row::Message::ContextMenu => {
                self.menu = Some(OpenMenu::Track(track_id));
                return;
            }
            track_row::Message::EditNote(note_id) => {
                return self.edit_note(track_id, &note_id, step)
            }
            track_row::Message::AddNote => {
                let at = ms(self.vm.position());
                return self.add_note_at(track_id, at, None, step);
            }
            track_row::Message::AddTechnique => {
                let at = ms(self.vm.position());
                return self.add_technique_at(track_id, at, None, step);
            }
            track_row::Message::ToggleMute => self.vm.mixer_do(|m, h| m.toggle_mute(h, id)),
            track_row::Message::ToggleSolo => self.vm.mixer_do(|m, h| m.toggle_solo(h, id)),
            track_row::Message::Volume(v) => {
                self.owe(Owed::TrackVolume(Source::Lane(Some(track_id.clone()))));
                self.vm.mixer_do(|m, h| m.set_track_volume_live(h, id, v))
            }
            track_row::Message::VolumeCommit => {
                self.owed
                    .retain(|o| !matches!(o, Owed::TrackVolume(Source::Lane(_))));
                self.vm.mixer_do(|m, _| m.commit_track_volume())
            }
            track_row::Message::ToggleExpand => {
                self.vm.analysis_do(|a, h| a.toggle_track_expanded(id, h))
            }
            track_row::Message::LaneDragStart => {
                self.lane_drag = Some(track_id);
                return;
            }
            track_row::Message::LaneDragEnd => {
                self.lane_drag = None;
                self.drop_target = None;
                return;
            }
            track_row::Message::LaneDragOver(over) => {
                if over {
                    self.drop_target = Some(track_id);
                } else if self.drop_target.as_deref() == Some(id) {
                    self.drop_target = None;
                }
                return;
            }
            track_row::Message::CrossLaneMove(dragged) => {
                self.drop_target = None;
                self.vm.move_vocal_track_to_lane_of(&dragged, id)
            }
        };
        self.absorb(fx, step);
    }

    fn lanes(&mut self, track_id: String, m: analysis_lanes::Message, step: &mut Step) {
        let fx = match m {
            analysis_lanes::Message::ToggleLane { track_id, lane } => self
                .vm
                .analysis_do(|a, h| a.toggle_lane(&track_id, lane, h)),
            analysis_lanes::Message::HealLane(id) => {
                self.vm.analysis_do(|a, h| a.heal_lane(&id, h))
            }
            analysis_lanes::Message::AddNote { start_ms, end_ms } => {
                return self.add_note_at(track_id, start_ms, end_ms, step);
            }
            analysis_lanes::Message::EditNote(note_id) => {
                return self.edit_note(track_id, &note_id, step)
            }
            analysis_lanes::Message::AddTechnique { start_ms, end_ms } => {
                return self.add_technique_at(track_id, start_ms, end_ms, step);
            }
            analysis_lanes::Message::EditTechnique(marker_id) => {
                return self.edit_technique(track_id, &marker_id, step);
            }
            analysis_lanes::Message::ImageRendered(done) => {
                if let Some(image) = self.lane_images.get_mut(&track_id) {
                    image.rendered(done);
                }
                return;
            }
        };
        self.absorb(fx, step);
    }

    /// The inspector's messages, and the dock summary's (`source` says
    /// which: they share the left panel's messages).
    fn inspector(&mut self, m: left_panel::Message, source: fn(String) -> Source, step: &mut Step) {
        if m.closes_menu() && matches!(self.menu, Some(OpenMenu::Inspector(_))) {
            self.menu = None;
        }
        use left_panel::Message as L;
        let fx = match m {
            L::ToggleInspectorPanel => self.vm.toggle_inspector_panel(),
            L::OpenMenu(menu) => {
                self.menu = Some(OpenMenu::Inspector(menu));
                return;
            }
            L::DismissMenu => return,
            L::HealSelectedAnalysis {
                spectrogram,
                metrics,
            } => self
                .vm
                .analysis_do(|a, h| a.heal_selected_analysis(spectrogram, metrics, h)),
            L::ShowDockTab(tab) => self.vm.analysis_do(|a, h| a.show_dock_tab(tab, h)),
            L::ToggleMute(id) => self.vm.mixer_do(|m, h| m.toggle_mute(h, &id)),
            L::ToggleSolo(id) => self.vm.mixer_do(|m, h| m.toggle_solo(h, &id)),
            L::SetTrackPanLive { track_id, pan } => {
                self.owe(Owed::TrackVolume(source(track_id.clone())));
                self.vm
                    .mixer_do(|m, h| m.set_track_pan_live(h, &track_id, pan))
            }
            L::SetTrackVolumeLive { track_id, volume } => {
                self.owe(Owed::TrackVolume(source(track_id.clone())));
                self.vm
                    .mixer_do(|m, h| m.set_track_volume_live(h, &track_id, volume))
            }
            L::CommitTrackVolume => {
                self.owed
                    .retain(|o| !matches!(o, Owed::TrackVolume(Source::Inspector(_))));
                self.vm.mixer_do(|m, _| m.commit_track_volume())
            }
            L::SetTrackBus { track_id, bus_id } => self
                .vm
                .mixer_do(|m, h| m.set_track_bus(h, &track_id, bus_id.as_deref())),
            L::AddTrackSend { track_id, bus_id } => self
                .vm
                .mixer_do(|m, h| m.add_track_send(h, &track_id, &bus_id)),
            L::RemoveTrackSend { track_id, slot } => self
                .vm
                .mixer_do(|m, h| m.remove_track_send(h, &track_id, slot)),
            L::SetTrackSendTarget {
                track_id,
                slot,
                bus_id,
            } => self
                .vm
                .mixer_do(|m, h| m.set_track_send_target(h, &track_id, slot, &bus_id)),
            L::SetTrackSendLevelLive {
                track_id,
                slot,
                level_db,
            } => {
                self.owe(Owed::Fx(source(track_id.clone())));
                self.vm.mixer_do(|m, h| {
                    m.set_track_send_level_live(h, &track_id, slot, level_db);
                })
            }
            L::ToggleBusMute(id) => self.vm.mixer_do(|m, h| m.toggle_bus_mute(h, &id)),
            L::ToggleBusSolo(id) => self.vm.mixer_do(|m, h| m.toggle_bus_solo(h, &id)),
            L::SetBusPanLive { bus_id, pan } => {
                self.owe(Owed::Fx(source(bus_id.clone())));
                self.vm.mixer_do(|m, h| m.set_bus_pan_live(h, &bus_id, pan))
            }
            L::SetBusVolumeLive { bus_id, volume } => {
                self.owe(Owed::Fx(source(bus_id.clone())));
                self.vm
                    .mixer_do(|m, h| m.set_bus_volume_live(h, &bus_id, volume))
            }
            L::SetBusOutput {
                bus_id,
                output_bus_id,
            } => self
                .vm
                .mixer_do(|m, h| m.set_bus_output(h, &bus_id, output_bus_id.as_deref())),
            L::AddBusSend {
                bus_id,
                target_bus_id,
            } => self
                .vm
                .mixer_do(|m, h| m.add_bus_send(h, &bus_id, &target_bus_id)),
            L::RemoveBusSend { bus_id, slot } => {
                self.vm.mixer_do(|m, h| m.remove_bus_send(h, &bus_id, slot))
            }
            L::SetBusSendTarget {
                bus_id,
                slot,
                target_bus_id,
            } => self
                .vm
                .mixer_do(|m, h| m.set_bus_send_target(h, &bus_id, slot, &target_bus_id)),
            L::SetBusSendLevelLive {
                bus_id,
                slot,
                level_db,
            } => {
                self.owe(Owed::Fx(source(bus_id.clone())));
                self.vm.mixer_do(|m, h| {
                    m.set_bus_send_level_live(h, &bus_id, slot, level_db);
                })
            }
            L::CommitFx => {
                self.owed
                    .retain(|o| !matches!(o, Owed::Fx(Source::Inspector(_))));
                self.vm.commit_fx()
            }
        };
        self.absorb(fx, step);
    }

    /// The FX panel's messages.
    fn fx(&mut self, m: fx_panel::Message, step: &mut Step) {
        let fx = match m {
            fx_panel::Message::SetTrackFxLive { id, fx } => {
                self.owe(Owed::Fx(Source::Inspector(id.clone())));
                self.vm.mixer_do(|m, h| m.set_track_fx_live(h, &id, fx))
            }
            fx_panel::Message::SetBusFxLive { id, fx } => {
                self.owe(Owed::Fx(Source::Inspector(id.clone())));
                self.vm.mixer_do(|m, h| m.set_bus_fx_live(h, &id, fx))
            }
            fx_panel::Message::CommitFx => {
                self.owed
                    .retain(|o| !matches!(o, Owed::Fx(Source::Inspector(_))));
                self.vm.commit_fx()
            }
            fx_panel::Message::ReorderTrackInserts { id, order } => self
                .vm
                .mixer_do(|m, h| m.reorder_track_inserts(h, &id, order)),
            fx_panel::Message::ReorderBusInserts { id, order } => self
                .vm
                .mixer_do(|m, h| m.reorder_bus_inserts(h, &id, order)),
        };
        self.absorb(fx, step);
    }

    /// The dock's mixer desk.
    fn mixer_desk(&mut self, m: mixer_strip::Message, step: &mut Step) {
        use mixer_strip::Message as X;
        let fx = match m {
            X::SelectTrack { id } => self.vm.select_track(&id),
            X::SelectBus { id } => {
                self.vm.select_bus(Some(&id));
                return;
            }
            X::ToggleMute { id } => self.vm.mixer_do(|m, h| m.toggle_mute(h, &id)),
            X::ToggleSolo { id } => self.vm.mixer_do(|m, h| m.toggle_solo(h, &id)),
            X::SetTrackVolumeLive { id, volume } => {
                self.owe(Owed::TrackVolume(Source::Mixer(Some(id.clone()))));
                self.vm
                    .mixer_do(|m, h| m.set_track_volume_live(h, &id, volume))
            }
            X::SetTrackPanLive { id, pan } => {
                self.owe(Owed::TrackVolume(Source::Mixer(Some(id.clone()))));
                self.vm.mixer_do(|m, h| m.set_track_pan_live(h, &id, pan))
            }
            X::CommitTrackVolume => {
                self.owed
                    .retain(|o| !matches!(o, Owed::TrackVolume(Source::Mixer(_))));
                self.vm.mixer_do(|m, _| m.commit_track_volume())
            }
            X::SetTrackBus { id, bus_id } => self
                .vm
                .mixer_do(|m, h| m.set_track_bus(h, &id, bus_id.as_deref())),
            X::DeleteTrack { id } => return self.delete_track(&id, step),
            X::ToggleBusMute { id } => self.vm.mixer_do(|m, h| m.toggle_bus_mute(h, &id)),
            X::ToggleBusSolo { id } => self.vm.mixer_do(|m, h| m.toggle_bus_solo(h, &id)),
            X::SetBusVolumeLive { id, volume } => {
                self.owe(Owed::Fx(Source::Mixer(Some(id.clone()))));
                self.vm
                    .mixer_do(|m, h| m.set_bus_volume_live(h, &id, volume))
            }
            X::SetBusPanLive { id, pan } => {
                self.owe(Owed::Fx(Source::Mixer(Some(id.clone()))));
                self.vm.mixer_do(|m, h| m.set_bus_pan_live(h, &id, pan))
            }
            X::CommitFx => {
                self.owed
                    .retain(|o| !matches!(o, Owed::Fx(Source::Mixer(_))));
                self.vm.commit_fx()
            }
            X::SetBusOutput { id, output_bus_id } => self
                .vm
                .mixer_do(|m, h| m.set_bus_output(h, &id, output_bus_id.as_deref())),
            X::RenameBus { id, current } => return self.rename_bus(id, &current, step),
            X::DeleteBus { id, name } => {
                self.ask(confirm_delete_bus(&name), Asked::DeleteBus(id), step);
                return;
            }
            X::CreateBus => self.vm.mixer_do(|m, h| m.create_bus(h, "")),
            X::SetMasterVolumeLive { gain } => {
                self.owe(Owed::MasterVolume(Source::Mixer(None)));
                self.vm.mixer_do(|m, h| m.set_master_volume_live(h, gain))
            }
            X::CommitMasterVolume => {
                self.owed
                    .retain(|o| *o != Owed::MasterVolume(Source::Mixer(None)));
                self.vm.mixer_do(|m, _| m.commit_master_volume())
            }
            X::SetRecordGainDbLive { db } => {
                self.vm.app_settings().set_record_gain_db_live(db);
                return;
            }
            X::SetMonitorGainLive { gain } => {
                self.vm.app_settings().set_monitor_gain_live(gain);
                return;
            }
            X::CommitAudioGains => {
                // Persisting the settings blocks: off the UI thread.
                let settings = std::sync::Arc::clone(self.vm.app_settings());
                step.task(Task::perform(
                    crate::util::blocking(move || settings.commit_audio_gains()),
                    |_| Message::Done,
                ));
                return;
            }
        };
        self.absorb(fx, step);
    }

    fn dock(&mut self, m: bottom_dock::Message, step: &mut Step) {
        let fx = match m {
            bottom_dock::Message::ShowDockTab(tab) => {
                self.vm.analysis_do(|a, h| a.show_dock_tab(tab, h))
            }
            bottom_dock::Message::ToggleDock => self.vm.analysis_do(|a, h| a.toggle_dock(h)),
            bottom_dock::Message::Resize { dy, cap } => self.vm.analysis_do(|a, _| {
                let height = resized_height(a.dock_height(), cap, dy);
                a.set_dock_height(height);
            }),
            bottom_dock::Message::Voiceprint(m) => match m {
                voiceprint::Message::Seek(to) => {
                    self.vm.seek(to);
                    return;
                }
                voiceprint::Message::SelectLive(on) => {
                    self.vm.analysis_do(|a, h| a.set_live_monitor(on, h))
                }
                voiceprint::Message::ImageRendered(done) => {
                    self.voiceprint.rendered(done);
                    return;
                }
            },
            bottom_dock::Message::Summary(m) => {
                return self.inspector(m, |id| Source::Mixer(Some(id)), step)
            }
            bottom_dock::Message::Mixer(m) => return self.mixer_desk(m, step),
        };
        self.absorb(fx, step);
    }

    fn lyrics_message(&mut self, m: lyrics_panel::Message, step: &mut Step) {
        use lyrics_panel::Message as P;
        let fx = match m {
            P::SetRightTab(tab) => {
                self.vm.set_right_tab(match tab {
                    lyrics_panel::RightTab::Lyrics => RightTab::Lyrics,
                    lyrics_panel::RightTab::Notes => RightTab::Notes,
                    lyrics_panel::RightTab::Techniques => RightTab::Techniques,
                });
                return;
            }
            P::ToggleLyricsPanel => {
                self.vm.toggle_lyrics_panel();
                return;
            }
            P::SetLyricsMode(mode) => {
                self.vm.set_lyrics_mode(match mode {
                    lyrics_panel::LyricsMode::Read => LyricsMode::Read,
                    lyrics_panel::LyricsMode::Edit => LyricsMode::Edit,
                    lyrics_panel::LyricsMode::Sync => LyricsMode::Sync,
                });
                return;
            }
            P::SetLyricsFocus(on) => {
                self.vm.set_lyrics_focus(on);
                return;
            }
            P::BrowseImport => {
                step.task(lyrics_panel::browse_lyrics_file().map(Message::Lyrics));
                return;
            }
            P::ImportLyricsFromFile(path) => {
                let ticket = {
                    let vm = &*self.vm;
                    let as_text = || vm.lyrics_as_text();
                    let data = panel_data(vm, &self.shown, &as_text, false);
                    self.lyrics.begin_import(&data)
                };
                let started = self.vm.import_lyrics_from_file(&path.to_string_lossy());
                self.await_call(started, Awaited::LyricsImport(ticket), step);
                return;
            }
            P::EditorAction(action) => {
                if let Some(text) = self.lyrics.perform(action) {
                    self.vm.set_lyrics_draft(Some(text));
                }
                return;
            }
            P::ApplyEditedLyrics => {
                let ticket = {
                    let vm = &*self.vm;
                    let as_text = || vm.lyrics_as_text();
                    let data = panel_data(vm, &self.shown, &as_text, false);
                    self.lyrics.save_ticket(&data)
                };
                if let Some(ticket) = ticket {
                    let started = self.vm.apply_edited_lyrics(ticket.text());
                    self.await_call(started, Awaited::LyricsSave(ticket), step);
                }
                return;
            }
            P::StampNextLyric => self.vm.stamp_next_lyric(),
            P::StampLyricAt(index) => self.vm.stamp_lyric_at(index),
            P::ClearLyricTimes => self.vm.clear_lyric_times(),
            P::SeekToLyric(line) => {
                self.vm.seek_to_lyric(&line);
                return;
            }
            P::Seek(to) => {
                self.vm.seek(to);
                return;
            }
            P::SelectTrack(id) => self.vm.select_track(&id),
            P::AddNote(track_id) => {
                let at = ms(self.vm.position());
                return self.add_note_at(track_id, at, None, step);
            }
            P::EditNote { track_id, note_id } => return self.edit_note(track_id, &note_id, step),
            P::AddTechnique(track_id) => {
                let at = ms(self.vm.position());
                return self.add_technique_at(track_id, at, None, step);
            }
            P::EditTechnique {
                track_id,
                marker_id,
            } => return self.edit_technique(track_id, &marker_id, step),
            P::Toast(text) => {
                self.toast(text, step);
                return;
            }
        };
        self.absorb(fx, step);
    }

    // ------------------------------------------------------------ drops

    /// `_guardedAudioDrop` and the zone's handler: never let a drop
    /// interleave with a recording finalize / separation / other busy
    /// mutation — both write the practice, so the import would clobber the
    /// just-recorded take (a lost update). Filters to supported audio, then
    /// imports it file by file.
    fn drop_files(
        &mut self,
        zone: DropZone,
        paths: Vec<PathBuf>,
        at_ms: Option<i64>,
        step: &mut Step,
    ) {
        // A take's drop lands at the playhead at the drop, or where the
        // caller took it earlier (the open panel can stay up for seconds
        // while playback runs on).
        let at = at_ms.unwrap_or_else(|| ms(self.vm.position()));
        // In the words every other refused edit uses ("Stop recording first",
        // "Wait for the take to finish saving", …).
        if self.refused(step) {
            return;
        }
        let paths: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let audio: VecDeque<String> = paths.iter().filter(|p| is_audio_path(p)).cloned().collect();
        if audio.is_empty() {
            if !paths.is_empty() {
                self.toast(NO_AUDIO_IN_DROP.to_string(), step);
            }
            return;
        }
        let id = self.next_run;
        self.next_run += 1;
        let files = audio.len();
        let kind = match zone {
            // The first file fills the empty backing slot (per the
            // Accompaniment / Original toggle); any remaining files become
            // new vocal takes.
            DropZone::Backing => {
                let reference = self.vm.mixer().play_reference();
                let practice = self.vm.practice();
                let empty = practice.is_some_and(|p| {
                    if reference {
                        p.reference.is_none()
                    } else {
                        p.accompaniment.is_none()
                    }
                });
                RunKind::Backing {
                    filled_slot: empty,
                    files,
                    counted: false,
                    slot: "",
                    before: 0,
                }
            }
            DropZone::NewTake => RunKind::NewTake,
            // Each file is added as a new clip on that take (never merged
            // into the existing clips), the first at the playhead and each
            // next one where the one before it ends, as a DAW lays out a
            // multi-file drop on one track. Positions come from the drop,
            // not from the playhead per file: each import waits for the one
            // before, and read per file, playback running on meanwhile spread
            // the files across the song. (Stacked at one spot instead, they
            // hid each other, and the lane's analysis kept only the last.)
            DropZone::Take(track_id) => RunKind::Clip {
                track_id,
                at_ms: at,
            },
        };
        let mut run = DropRun {
            id,
            kind,
            queue: audio,
        };
        if let RunKind::Backing {
            filled_slot: true, ..
        } = run.kind
        {
            let Some(first) = run.queue.pop_front() else {
                return;
            };
            self.runs.push(run);
            let started = if self.vm.mixer().play_reference() {
                self.vm.add_reference(&first)
            } else {
                self.vm.add_accompaniment(&first)
            };
            self.await_call(started, Awaited::Drop(id), step);
            return;
        }
        self.runs.push(run);
        self.continue_run(id, step);
    }

    /// The next file of drop `id`, or its end.
    fn continue_run(&mut self, id: u64, step: &mut Step) {
        let Some(i) = self.runs.iter().position(|r| r.id == id) else {
            return;
        };
        let vocal_count = self.vm.practice().map_or(0, |p| p.vocal_tracks.len());
        let reference = self.vm.mixer().play_reference();
        let run = &mut self.runs[i];
        if let RunKind::Backing {
            counted,
            slot,
            before,
            ..
        } = &mut run.kind
        {
            // Read once the backing (if any) landed, as Dart read them after
            // its first await.
            if !*counted {
                *counted = true;
                *slot = if reference {
                    "Original"
                } else {
                    "Accompaniment"
                };
                *before = vocal_count;
            }
        }
        let Some(path) = run.queue.pop_front() else {
            let run = self.runs.remove(i);
            // The lane lit up as the target, and the files then landed as
            // takes with nothing said. Said once they have all landed: said
            // first, an import's own toast replaced it, and a failed import
            // made it untrue (that import's toast says so instead).
            if let RunKind::Backing {
                filled_slot: false,
                files,
                slot,
                before,
                ..
            } = run.kind
            {
                if vocal_count.saturating_sub(before) == files {
                    self.toast(backing_set_toast(slot), step);
                }
            }
            return;
        };
        let started = match &run.kind {
            RunKind::Backing { .. } | RunKind::NewTake => self.vm.add_vocal_take(&path),
            RunKind::Clip { track_id, at_ms } => {
                let (track_id, at_ms) = (track_id.clone(), *at_ms);
                self.vm.add_clip_to_take(&track_id, &path, Some(at_ms))
            }
        };
        self.await_call(started, Awaited::Drop(id), step);
    }

    // ------------------------------------------------------- the editors

    /// `_addNoteAt`: adds a note at `start_ms` (a point) or across
    /// `start_ms..end_ms` (a range) — the lane passes a range when the user
    /// drags; the header, the menu and the right panel add one at the
    /// playhead.
    fn add_note_at(
        &mut self,
        track_id: String,
        start_ms: i64,
        end_ms: Option<i64>,
        step: &mut Step,
    ) {
        let editor = NoteEditor::new(
            None,
            millis(start_ms),
            end_ms.map(millis),
            normalize_marker_range,
        );
        step.task(editor.focus());
        self.note_editor = Some((
            editor,
            NoteTarget::Add {
                track_id,
                start_ms,
                end_ms,
            },
        ));
    }

    /// `_editNote`.
    fn edit_note(&mut self, track_id: String, note_id: &str, step: &mut Step) {
        let Some(note) = self
            .vm
            .track_by_id(&track_id)
            .and_then(|t| t.notes.iter().find(|n| n.id == note_id).cloned())
        else {
            return;
        };
        let editor = NoteEditor::new(
            Some(&note),
            self.vm.position(),
            None,
            normalize_marker_range,
        );
        step.task(editor.focus());
        self.note_editor = Some((
            editor,
            NoteTarget::Edit {
                track_id,
                note_id: note.id,
            },
        ));
    }

    fn note_editor_message(&mut self, m: note_editor::Message, step: &mut Step) {
        let Some((editor, _)) = self.note_editor.as_mut() else {
            return;
        };
        let note_editor::Step::Closed(result) = editor.update(m) else {
            return;
        };
        let Some((_, target)) = self.note_editor.take() else {
            return;
        };
        let Some(result) = result else {
            return;
        };
        let fx = match target {
            NoteTarget::Add {
                track_id,
                start_ms,
                end_ms,
            } => match result.content {
                Some(content) => self.vm.add_note(&track_id, start_ms, end_ms, &content),
                None => return,
            },
            NoteTarget::Edit { track_id, note_id } => {
                if result.deleted {
                    self.vm.delete_note(&track_id, &note_id)
                } else if let Some(content) = result.content {
                    self.vm.update_note_content(&track_id, &note_id, &content)
                } else {
                    return;
                }
            }
        };
        self.absorb(fx, step);
    }

    /// `_addTechniqueAt`: the picker, titled as the technique will be kept
    /// (see the note editor), then the technique at `start_ms` (a point) or
    /// across `start_ms..end_ms`.
    fn add_technique_at(
        &mut self,
        track_id: String,
        start_ms: i64,
        end_ms: Option<i64>,
        step: &mut Step,
    ) {
        let (s, e) = normalize_marker_range(start_ms, end_ms);
        let title = match e {
            Some(e) => format!(
                "Add technique across {} → {}",
                format_clock_ms(s),
                format_clock_ms(e)
            ),
            None => format!("Add technique at {}", format_clock_ms(s)),
        };
        self.picker_seq += 1;
        let seq = self.picker_seq;
        let picker = TechniquePicker::new(title);
        step.task(picker.focus());
        self.picker = Some(OpenPicker {
            picker,
            seq,
            track_id,
            start_ms,
            end_ms,
        });
        let started = self.vm.available_techniques();
        self.await_call(started, Awaited::Techniques(seq), step);
    }

    fn picker_message(&mut self, m: technique_picker::Message, step: &mut Step) {
        let Some(open) = self.picker.as_mut() else {
            return;
        };
        let Some(picked) = open.picker.update(m) else {
            return;
        };
        let Some(open) = self.picker.take() else {
            return;
        };
        if let Some(t) = picked {
            let fx =
                self.vm
                    .add_technique(&open.track_id, &t.id, &t.name, open.start_ms, open.end_ms);
            self.absorb(fx, step);
        }
    }

    /// `_editTechnique`: asks to remove the marker.
    fn edit_technique(&mut self, track_id: String, marker_id: &str, step: &mut Step) {
        let Some(marker) = self.vm.track_by_id(&track_id).and_then(|t| {
            t.technique_markers
                .iter()
                .find(|m| m.id == marker_id)
                .cloned()
        }) else {
            return;
        };
        let dialog = ConfirmDialog::new(format!("Remove \"{}\"?", marker.technique_name))
            .body("This removes the technique marker from this take. This cannot be undone.")
            .confirm_label("Remove");
        self.ask(
            dialog,
            Asked::RemoveTechnique {
                track_id,
                marker_id: marker.id,
            },
            step,
        );
    }

    // ---------------------------------------------- confirmations, prompts

    /// `_deleteTrack`: not asked to confirm an edit that would then be
    /// refused.
    fn delete_track(&mut self, track_id: &str, step: &mut Step) {
        if self.refused(step) {
            return;
        }
        let Some(track) = self.vm.track_by_id(track_id) else {
            return;
        };
        let dialog = ConfirmDialog::new(format!("Delete \"{}\"?", track.display_name))
            .body("This removes the track, its audio file and its notes.");
        self.ask(dialog, Asked::DeleteTrack(track.id.clone()), step);
    }

    /// `_deleteClip`: asked first, as Delete take is: the clip's audio file
    /// goes with it, and on a one-clip take that is all the take's audio and
    /// its notes.
    fn delete_clip(&mut self, track_id: String, clip_id: String, step: &mut Step) {
        if self.refused(step) {
            return;
        }
        let dialog = ConfirmDialog::new("Delete this clip?")
            .body("This removes the clip, its audio file and the notes inside it.");
        self.ask(dialog, Asked::DeleteClip { track_id, clip_id }, step);
    }

    /// `_renameTrack`: offers the take's name as it is now.
    fn rename_track(&mut self, track_id: &str, step: &mut Step) {
        let Some(track) = self.vm.track_by_id(track_id) else {
            return;
        };
        self.ask(
            rename_dialog(&track.display_name),
            Asked::RenameTrack(track.id.clone()),
            step,
        );
    }

    /// `_separateBacking`.
    fn separate(&mut self, track_id: &str, step: &mut Step) {
        if self.refused(step) {
            return; // as _deleteTrack
        }
        let reseparate = self.vm.can_reseparate();
        // A backing the user imported is replaced too (with its notes and
        // mix), and the first-extraction copy did not say so.
        let replaces_import = !reseparate
            && self
                .vm
                .practice()
                .is_some_and(|p| p.accompaniment.is_some());
        let dialog = ConfirmDialog::new(if reseparate {
            "Re-extract the backing?"
        } else {
            "Extract the backing?"
        })
        .icon(crate::ui::core::icons::Icon::WandSparkles)
        .destructive(replaces_import)
        .body(if reseparate {
            "Runs separation on the original again. The instrumental in the Accompaniment slot is \
             replaced. This takes a minute or two."
        } else if replaces_import {
            "Pulls the instrumental out of the song into the Accompaniment slot, replacing the \
             backing track there, with its notes. The Original stays put. This takes a minute or \
             two."
        } else {
            "Pulls the instrumental out of the song into the Accompaniment slot to sing over. The \
             Original stays put. This takes a minute or two."
        })
        .confirm_label(if reseparate {
            "Re-extract"
        } else if replaces_import {
            "Replace backing"
        } else {
            "Extract backing"
        });
        self.ask(dialog, Asked::Separate(track_id.to_string()), step);
    }

    fn rename_bus(&mut self, bus_id: String, current: &str, step: &mut Step) {
        let mut prompt = rename_dialog(current);
        prompt.title = "Rename bus".to_string();
        prompt.hint = "Bus name".to_string();
        self.ask(prompt, Asked::RenameBus(bus_id), step);
    }

    fn answered(&mut self, asked: Asked, result: DialogResult, step: &mut Step) {
        self.dialogs_open = self.dialogs_open.saturating_sub(1);
        let confirmed = result.confirmed();
        let fx = match asked {
            Asked::DeleteTrack(id) if confirmed => self.vm.delete_track(&id),
            Asked::DeleteClip { track_id, clip_id } if confirmed => {
                self.vm.delete_clip(&track_id, &clip_id)
            }
            Asked::RenameTrack(id) => match result.text() {
                Some(name) => self.vm.rename_track(&id, &name),
                None => return,
            },
            Asked::Separate(id) if confirmed => self.vm.separate_backing(&id),
            Asked::RemoveTechnique {
                track_id,
                marker_id,
            } if confirmed => self.vm.delete_technique_marker(&track_id, &marker_id),
            Asked::RenameBus(id) => match result.text() {
                Some(name) => self.vm.mixer_do(|m, h| m.rename_bus(h, &id, &name)),
                None => return,
            },
            Asked::DeleteBus(id) if confirmed => self.vm.mixer_do(|m, h| m.delete_bus(h, &id)),
            Asked::NewInterpretation => match result.text() {
                Some(name) => self.vm.versions_do(|v, h| v.create(&name, h)),
                None => return,
            },
            Asked::RenameInterpretation(id) => match result.text() {
                Some(name) => self.vm.versions_do(|v, h| v.rename(&id, &name, h)),
                None => return,
            },
            Asked::DeleteInterpretation(id) if confirmed => {
                self.vm.versions_do(|v, h| v.delete(&id, h))
            }
            _ => return,
        };
        self.absorb(fx, step);
    }

    // ------------------------------------------------------------- menus

    /// A row of the lane menu: the menu closes, then the action runs on the
    /// take as the practice holds it now (a lane body kept from before a
    /// rename handed in the take as it was: the Rename dialog then offered
    /// the old name).
    fn track_action(&mut self, action: TrackAction, step: &mut Step) {
        let Some(OpenMenu::Track(id)) = self.menu.take() else {
            return;
        };
        let Some(track) = self.vm.track_by_id(&id) else {
            return;
        };
        let fx = match action {
            TrackAction::PickColor(value) => self.vm.set_track_color(&id, Some(value)),
            TrackAction::Rename => return self.rename_track(&id, step),
            TrackAction::Separate => return self.separate(&id, step),
            TrackAction::Export => self.vm.export_track_audio(&id),
            TrackAction::AddNote => {
                let at = ms(self.vm.position());
                return self.add_note_at(id, at, None, step);
            }
            TrackAction::AddTechnique => {
                let at = ms(self.vm.position());
                return self.add_technique_at(id, at, None, step);
            }
            TrackAction::AddClip => {
                // Refused before the open panel, not after a file was picked
                // in it.
                if self.refused(step) {
                    return;
                }
                // "At playhead" means where it was at the tap: the open
                // panel can stay up for seconds while playback runs on.
                let at = ms(self.vm.position());
                step.picks
                    .push((DropZone::Take(track.id.clone()), Some(at)));
                return;
            }
            TrackAction::Duplicate => self.vm.duplicate_track(&id),
            TrackAction::Split => self.vm.split_track_at_playhead(&id),
            TrackAction::MergeDown => self.vm.merge_track_down(&id),
            TrackAction::Reanalyze => self.vm.reanalyze_track(&id),
            TrackAction::Delete => return self.delete_track(&id, step),
        };
        self.absorb(fx, step);
    }

    /// A row of the clip menu. A backing lane is its one clip: deleting it
    /// deletes the lane, so it asks as Delete does (the clip's dialog spoke
    /// of the clip alone).
    fn clip_action(&mut self, action: ClipAction, step: &mut Step) {
        let Some(OpenMenu::Clip { track_id, clip_id }) = self.menu.take() else {
            return;
        };
        let Some(track) = self.vm.track_by_id(&track_id) else {
            return;
        };
        match action {
            ClipAction::Delete if track.is_backing() => self.delete_track(&track_id, step),
            ClipAction::Delete => self.delete_clip(track_id, clip_id, step),
            ClipAction::Export => {
                let fx = self.vm.export_clip_audio(&track_id, &clip_id);
                self.absorb(fx, step);
            }
        }
    }

    /// A row of a bus lane's menu.
    fn bus_action(&mut self, action: BusAction, step: &mut Step) {
        let Some(OpenMenu::Bus(id)) = self.menu.take() else {
            return;
        };
        let Some(bus) = self
            .vm
            .practice()
            .and_then(|p| p.fx_buses.iter().find(|b| b.id == id).cloned())
        else {
            return;
        };
        let fx = match action {
            BusAction::PickColor(value) => self.vm.mixer_do(|m, h| m.set_bus_color(h, &id, value)),
            BusAction::Rename => return self.rename_bus(id, &bus.name, step),
            BusAction::Duplicate => self.vm.mixer_do(|m, h| m.duplicate_bus(h, &id)),
            BusAction::Delete => {
                self.ask(confirm_delete_bus(&bus.name), Asked::DeleteBus(id), step);
                return;
            }
        };
        self.absorb(fx, step);
    }

    /// A row of the interpretation dropdown.
    fn interpretation_action(&mut self, action: InterpretationAction, step: &mut Step) {
        if self.menu.take() != Some(OpenMenu::Interpretations) {
            return;
        }
        let active = self.vm.practice().and_then(|p| p.active_version().cloned());
        match action {
            InterpretationAction::SetActive(id) => {
                let fx = self.vm.versions_do(|v, h| v.set_active(&id, h));
                self.absorb(fx, step);
            }
            InterpretationAction::Create => {
                // Not asked, then refused.
                let mut refuses = false;
                let fx = self.vm.versions_do(|v, h| {
                    refuses = v.refuses_create(h);
                    Vec::new()
                });
                self.absorb(fx, step);
                if refuses {
                    return;
                }
                let next = crate::ui::song_practice_detail::view_model::interpretations_controller::InterpretationsController::next_default_name(self.vm.practice());
                let mut prompt = rename_dialog(&next);
                prompt.title = "New interpretation".to_string();
                prompt.hint = "e.g. Tender, Powerful".to_string();
                prompt.confirm_label = "Create".to_string();
                self.ask(prompt, Asked::NewInterpretation, step);
            }
            InterpretationAction::Rename => {
                let Some(a) = active else {
                    return;
                };
                let mut prompt = rename_dialog(&a.name);
                prompt.title = "Rename interpretation".to_string();
                prompt.hint = "Interpretation name".to_string();
                self.ask(prompt, Asked::RenameInterpretation(a.id.clone()), step);
            }
            InterpretationAction::Delete => {
                let Some(a) = active else {
                    return;
                };
                // Not asked, then refused.
                if self.refused(step) {
                    return;
                }
                let dialog = ConfirmDialog::new(format!("Delete \"{}\"?", a.name))
                    .body("This removes the interpretation and all of its takes.");
                self.ask(dialog, Asked::DeleteInterpretation(a.id.clone()), step);
            }
        }
    }

    // ------------------------------------------------- after every update

    /// What Dart's rebuild did, after the frame: the engine's events, the
    /// heals the dock and the inspector asked, the FX panel's readings, the
    /// spectrogram images and the lyrics panel.
    pub(super) fn refresh(&mut self, step: &mut Step) {
        self.vm.drain_engine_events();
        if self.vm.practice().is_none() {
            self.shown = super::Shown::default();
            self.lane_images.clear();
            self.voiceprint.clear();
            return;
        }
        self.settle_owed(false, step);
        // The dock asks again for what its tab shows that the cache let go.
        let dock_heals = self.vm.with_analysis(|a, h| {
            bottom_dock::heals(
                a.dock_open(),
                a.dock_shows_take(),
                a.live_monitor(),
                a.selected_analysis_incomplete(h),
            )
        });
        if dock_heals {
            let fx = self
                .vm
                .analysis_do(|a, h| a.heal_selected_analysis(None, None, h));
            self.absorb(fx, step);
        }
        // The inspector is always visible, so it asks for the metrics it
        // shows itself.
        let lacks_metrics = self
            .vm
            .with_analysis(|a, h| a.lacks_analysis(false, true, h));
        let selected = self.vm.selected_track();
        let heal = Inspector {
            open: self.vm.inspector_open(),
            selected_track: selected.as_deref(),
            selected_bus: None,
            fx_buses: &[],
            supports_effects: false,
            vocal_takes: &[],
            active_interpretation: None,
            open_menu: None,
        }
        .analysis_heal(lacks_metrics);
        if let Some(left_panel::Message::HealSelectedAnalysis {
            spectrogram,
            metrics,
        }) = heal
        {
            let fx = self
                .vm
                .analysis_do(|a, h| a.heal_selected_analysis(spectrogram, metrics, h));
            self.absorb(fx, step);
        }
        self.shown.read(&self.vm);
        self.read_fx_panel(step);
        self.sync_images(step);
        self.sync_lyrics(step);
    }

    /// The FX panel's EQ analyzer input and compressor reading, for the
    /// channel the inspector shows (the panel polled them in Dart).
    fn read_fx_panel(&mut self, step: &mut Step) {
        self.shown.eq_input = None;
        self.shown.gain_reduction_db = 0.0;
        if !self.vm.inspector_open() || !self.vm.mixer().supports_effects() {
            return;
        }
        if let Some(bus) = self.vm.selected_bus().cloned() {
            self.shown.gain_reduction_db = self.vm.mixer().bus_gain_reduction_db(&bus.id);
            return;
        }
        let Some(track) = self.vm.selected_track() else {
            return;
        };
        if track.is_backing() {
            return;
        }
        let id = track.id.clone();
        self.shown.gain_reduction_db = self.vm.with_mixer(|m, h| m.gain_reduction_db(h, &id));
        // Only the EQ's analyzer reads the input, and only while the EQ is
        // on (its editor is shown).
        if !track.fx.as_ref().is_some_and(|fx| fx.eq_on) {
            return;
        }
        let mut column = None;
        let fx = self.vm.analysis_do(|a, h| {
            let (c, effects) = a.eq_input_column(&id, h);
            column = c;
            effects
        });
        self.shown.eq_input = column;
        self.absorb(fx, step);
    }

    /// The dock's Spectrogram image and each folded-out take's spectrogram
    /// lane image.
    fn sync_images(&mut self, step: &mut Step) {
        let (dock_open, dock_tab) = (
            self.vm.analysis().dock_open(),
            self.vm.analysis().dock_tab(),
        );
        if dock_open && dock_tab == DockTab::Spectrogram {
            if let Some(render) = self.voiceprint.sync_request(&*self.vm) {
                step.renders.push((Render::Dock, render));
            }
        } else {
            self.voiceprint.clear();
        }
        let mut kept = HashSet::new();
        for take in &self.shown.vocal_takes {
            let id = take.id.as_str();
            if !self.vm.analysis().is_track_expanded(id)
                || !self
                    .vm
                    .analysis()
                    .lanes_for(id)
                    .contains(&analysis_lanes::AnalysisLane::Spectrogram)
            {
                continue;
            }
            // The lane shows the spectrogram only once it is not loading.
            let spec = self.vm.with_analysis(|a, h| {
                (!a.spectrogram_loading_for(id, h))
                    .then(|| a.spectrogram_for(id, h))
                    .flatten()
            });
            let image = self.lane_images.entry(id.to_string()).or_default();
            if let Some(render) = image.sync_request(spec.as_ref()) {
                step.renders.push((Render::Lane(id.to_string()), render));
            }
            kept.insert(id.to_string());
        }
        self.lane_images.retain(|id, _| kept.contains(id));
    }

    fn sync_lyrics(&mut self, step: &mut Step) {
        let under_dialog = self.dialogs_open > 0
            || self.note_editor.is_some()
            || self.picker.is_some()
            || self.menu.is_some();
        let vm = &*self.vm;
        let as_text = || vm.lyrics_as_text();
        let data = panel_data(vm, &self.shown, &as_text, under_dialog);
        let task = self.lyrics.sync(&data);
        step.task(task.map(Message::Lyrics));
    }

    /// Commits a live edit whose control left the screen mid-gesture (`all`:
    /// the page itself is leaving), as Dart's controls committed from their
    /// `dispose`; and a clip move-drag whose take is no longer shown.
    pub(super) fn settle_owed(&mut self, all: bool, step: &mut Step) {
        let owed = std::mem::take(&mut self.owed);
        let mut kept = Vec::new();
        for o in owed {
            let source = match &o {
                Owed::TrackVolume(s) | Owed::Fx(s) | Owed::MasterVolume(s) => s,
            };
            if !all && self.source_shown(source) {
                kept.push(o);
                continue;
            }
            let fx = match o {
                Owed::TrackVolume(_) => self.vm.mixer_do(|m, _| m.commit_track_volume()),
                Owed::Fx(_) => self.vm.commit_fx(),
                Owed::MasterVolume(_) => self.vm.mixer_do(|m, _| m.commit_master_volume()),
            };
            self.absorb(fx, step);
        }
        self.owed = kept;
        if let Some(take) = self.moving.clone() {
            let shown = !all && self.lane_shown(&take);
            if !shown {
                self.moving = None;
                // A cancelled drag has already moved the model: skipping the
                // commit would leave persistence and the engine silently
                // diverged from what is on screen.
                if self.vm.is_moving_live(&take) {
                    let fx = self.vm.commit_track_move();
                    self.absorb(fx, step);
                }
            }
        }
    }

    /// Whether a timeline lane for `id` (a take, the backing or a bus) is on
    /// screen.
    fn lane_shown(&self, id: &str) -> bool {
        let Some(practice) = self.vm.practice() else {
            return false;
        };
        self.vm.active_backing().is_some_and(|b| b.id == id)
            || practice.active_vocal_tracks().iter().any(|t| t.id == id)
            || (self.vm.mixer().supports_effects() && practice.fx_buses.iter().any(|b| b.id == id))
    }

    fn source_shown(&self, source: &Source) -> bool {
        match source {
            Source::Lane(None) => self.vm.practice().is_some(),
            Source::Lane(Some(id)) => self.lane_shown(id),
            Source::Inspector(id) => {
                self.vm.inspector_open()
                    && match self.vm.selected_bus() {
                        Some(bus) => bus.id == *id,
                        None => self.vm.selected_track_id() == Some(id.as_str()),
                    }
            }
            Source::Mixer(id) => {
                let a = self.vm.analysis();
                a.dock_open()
                    && a.dock_tab() == DockTab::Mixer
                    && id.as_ref().is_none_or(|id| {
                        let practice = self.vm.practice();
                        self.vm.active_backing().is_some_and(|b| b.id == *id)
                            || practice.is_some_and(|p| {
                                p.active_vocal_tracks().iter().any(|t| t.id == *id)
                                    || (self.vm.mixer().supports_effects()
                                        && p.fx_buses.iter().any(|b| b.id == *id))
                            })
                    })
            }
        }
    }
}

/// `Duration(milliseconds: ms)`, never before 0.
fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}
