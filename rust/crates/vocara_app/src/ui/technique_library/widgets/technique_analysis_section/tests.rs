//! The analysis card on screen, headless over the view model tests' fakes:
//! `technique_practice_failure_test`'s widget cases (the screen part its
//! view-model port left), and the card's own states — the box's loading,
//! retry and empty cases, the gap table, the quality warning, the
//! dimension bar.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use iced::Size;
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_services::engine::media::MediaError;

use super::*;
use crate::ui::core::icons;
use crate::ui::core::theme::font;
use crate::ui::technique_library::view_model::technique_detail_view_model::tests::support::{
    canned_analysis, example, twang, Outcome, Rig, Setup, StartMode,
};
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

const SIZE: Size = Size::new(960.0, 1200.0);

fn settings() -> iced::Settings {
    let mut settings = crate::app::settings();
    settings.fonts = font::FACES
        .iter()
        .copied()
        .chain([icons::FONT_BYTES])
        .map(Cow::Borrowed)
        .collect();
    settings
}

/// A text containing `part`.
fn containing(part: &str) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let part = part.to_string();
    move |candidate: Candidate<'_>| match &candidate {
        Candidate::Text { content, .. } if content.contains(part.as_str()) => {
            Some(Target::from(candidate))
        }
        _ => None,
    }
}

/// The card over a view model, as the read view (`allow_practice`) or the
/// editor shows it.
struct Card {
    rig: Rig,
    image: VoiceprintState,
    allow_practice: bool,
}

impl Card {
    fn new(rig: Rig) -> Self {
        Self {
            rig,
            image: VoiceprintState::new(),
            allow_practice: true,
        }
    }

    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(
            settings(),
            SIZE,
            view(&self.rig.vm, &self.image, self.allow_practice),
        )
    }

    fn shows(&self, part: &str) -> bool {
        self.ui().find(containing(part)).is_ok()
    }

    /// What clicking the text `label` reports, not handled.
    fn press(&self, label: &str) -> Vec<Message> {
        let mut ui = self.ui();
        ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
        ui.into_messages().collect()
    }

    fn send(&mut self, message: Message) {
        let next = update(
            &mut self.rig.vm,
            &mut self.image,
            message,
            self.allow_practice,
        );
        self.rig.apply(next);
    }

    fn click(&mut self, label: &str) {
        for m in self.press(label) {
            self.send(m);
        }
    }
}

/// The reference analyzes fine; the practice take (> 10 min) trips the cap
/// (`technique_practice_failure_test`'s fakes).
fn practice_failure() -> Setup {
    let s = Setup::new(twang("T1", vec![])).analysis(Outcome::Fresh);
    let root = s.root().to_path_buf();
    s.repo.st().technique.as_mut().unwrap().media =
        vec![example("E1", &root.join("E1.wav").to_string_lossy())];
    s.analysis.st().refuse_paths = Some((
        "practice_take".into(),
        MediaError::TooLong {
            max_duration: Duration::from_secs(10 * 60),
        },
    ));
    s.recorder.st().mode = StartMode::In(root);
    s
}

// technique_practice_failure_test: 'a failed practice analysis is shown on
// its own panel'. A take the analysis refuses (here: past the 10-minute
// cap) used to snap the panel back to the reference, with the practice
// segment disabled: the cause was kept but shown nowhere.
#[test]
fn a_failed_practice_analysis_is_shown_on_its_own_panel() {
    let s = practice_failure();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    rig.vm.set_analysis_dim(8); // Range
    let mut card = Card::new(rig);
    assert!(card.shows(RECORD_PRACTICE));

    card.click(RECORD_PRACTICE);
    card.rig.settle();
    assert_eq!(card.rig.take_answers(), [Answer::PracticeStart(None)]);
    card.click(STOP);
    card.rig.settle();

    // The failed take stays the subject, and its panel says why.
    assert!(card.rig.vm.practice_analysis_failed());
    assert_eq!(
        card.rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert!(card.shows("too long"));

    // The user can leave it and come back to read it again.
    card.click(REFERENCE);
    assert_eq!(
        card.rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
    assert!(!card.shows("too long"));
    card.click(YOUR_PRACTICE);
    assert_eq!(
        card.rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert!(card.shows("too long"));
}

// technique_practice_failure_test: 'the practice button says the mic is
// starting, and takes no taps'. It said "Record practice" through the
// permission prompt and the mic's opening, and every tap did nothing,
// unlike the example's record row.
#[test]
fn the_practice_button_says_the_mic_is_starting_and_takes_no_taps() {
    let s = practice_failure();
    let mut rig = s.loaded();
    rig.vm.set_analysis_dim(8);
    rig.hold(JobKind::StartRecording);
    let mut card = Card::new(rig);
    assert!(card.shows(RECORD_PRACTICE));

    card.click(RECORD_PRACTICE);
    assert!(card.shows(STARTING));
    assert!(!card.shows(RECORD_PRACTICE));
    assert!(card.press(STARTING).is_empty(), "no taps");
    // A start that reaches the view model anyway starts nothing more.
    card.send(Message::Record);
    assert_eq!(card.rig.queued(JobKind::StartRecording), 1);

    card.rig.release(JobKind::StartRecording);
    assert!(!card.rig.vm.starting_practice());
    assert!(card.shows(STOP));
    card.rig
        .call_settled(|vm| vm.stop_practice_skipping_analysis());
    assert!(!card.rig.vm.is_recording_practice());
}

// The editor's card: the dimension tabs and the box, no practice recorder
// or subject switch (there you build the examples, not practice against
// them).
#[test]
fn the_editor_card_offers_no_practice() {
    let s = Setup::new(twang("T1", vec![example("E1", "/tmp/e1.wav")])).analysis(Outcome::Fresh);
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    let mut card = Card::new(rig);
    card.allow_practice = false;
    assert!(card.shows("Spectrogram"));
    assert!(!card.shows(RECORD_PRACTICE));
    assert!(!card.shows(YOUR_PRACTICE));
    card.send(Message::Record);
    card.rig.settle();
    assert!(
        !card.rig.vm.is_recording_practice(),
        "a stray tap starts nothing"
    );
}

// The box's own states: a spinner while the example analyzes, then the
// view; an analysis that may yet succeed offers "Try again"; one refused
// for what the clip is says so, with nothing to retry.
#[test]
fn the_box_says_what_it_is_waiting_for_and_why_it_has_nothing() {
    let s = Setup::new(twang("T1", vec![example("E1", "/tmp/e1.wav")]));
    let mut rig = s.loaded();
    rig.hold(JobKind::AnalyzeReference);
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(box_body(&rig.vm), BoxBody::Loading(ANALYZING_EXAMPLE));
    let card = Card::new(rig);
    assert!(card.shows(ANALYZING_EXAMPLE));

    // The analysis produced nothing (a decode hiccup): try again.
    let mut rig = card.rig;
    rig.release(JobKind::AnalyzeReference);
    assert_eq!(box_body(&rig.vm), BoxBody::Retry(EXAMPLE_FAILED.into()));
    let mut card = Card::new(rig);
    assert!(card.shows(EXAMPLE_FAILED));
    s.analysis.st().outcome = Outcome::Fresh;
    card.click(TRY_AGAIN);
    card.rig.settle();
    assert!(card.rig.vm.reference_analysis().is_some());
    assert_eq!(box_body(&card.rig.vm), BoxBody::View);
    assert!(!card.shows(TRY_AGAIN));

    // A format the engine cannot decode is said, with no retry.
    let s = Setup::new(twang("T1", vec![example("E1", "/tmp/e1.opus")]));
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(
        box_body(&rig.vm),
        BoxBody::Hint("This clip plays, but its format can't be analyzed.".into())
    );

    // No take yet on the practice side.
    let s = Setup::new(twang("T1", vec![example("E1", "/tmp/e1.wav")])).analysis(Outcome::Fresh);
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    rig.vm
        .show_analysis_subject(TechniqueAnalysisSubject::Practice);
    assert_eq!(box_body(&rig.vm), BoxBody::Hint(RECORD_YOURSELF.into()));
}

// The dimension bar: the Dart order, the active one kept on the view model,
// and a stray index ignored.
#[test]
fn the_dimension_bar_lists_the_views_in_order() {
    assert_eq!(
        DIMENSIONS,
        [
            "Spectrogram",
            "Power",
            "Vowel",
            "Pitch",
            "Timbre",
            "LTAS",
            "Vibrato",
            "Register",
            "Range",
            "Dynamics"
        ]
    );
    let s = Setup::new(twang("T1", vec![]));
    let mut card = Card::new(s.loaded());
    card.click("Vibrato");
    assert_eq!(card.rig.vm.analysis_dim(), 6);
    card.send(Message::Dimension(DIMENSIONS.len()));
    assert_eq!(card.rig.vm.analysis_dim(), 6);
}

fn analysis_with(edit: impl FnOnce(&mut VoiceMetrics)) -> TechniqueAnalysis {
    let mut a = canned_analysis();
    let mut metrics = VoiceMetrics::clone(&a.metrics);
    edit(&mut metrics);
    a.metrics = Arc::new(metrics);
    a
}

// `_GapTable`: the timbre averages always; the singer's formant, the range
// and the dynamics where both takes have them; each gap signed, with its
// unit, green within its tolerance.
#[test]
fn the_gap_table_compares_the_metrics_both_takes_have() {
    let reference = analysis_with(|m| {
        m.ring_avg = 10.0;
        m.tilt_avg = -12.0;
        m.cpp_avg = f64::NAN;
    });
    let practice = analysis_with(|m| {
        m.ring_avg = 13.04;
        m.tilt_avg = -12.5;
        m.cpp_avg = 8.0;
    });
    let rows = gap_rows(&reference, &practice);
    let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
    assert_eq!(
        labels,
        [
            "RING",
            "TILT",
            "CPP",
            "SINGER'S FORMANT",
            "RANGE",
            "DYNAMICS"
        ]
    );

    let ring = &rows[0];
    assert_eq!(ring.value_text(ring.reference), "10.0 dB");
    assert_eq!(ring.delta_text(), "+3.0 dB");
    assert!(!ring.close(), "3 dB is past the 2 dB tolerance");
    let tilt = &rows[1];
    assert_eq!(tilt.delta_text(), "−0.5 dB");
    assert!(tilt.close());
    let cpp = &rows[2];
    assert_eq!(cpp.value_text(cpp.reference), "— dB");
    assert_eq!(cpp.delta_text(), "—");
    assert!(!cpp.has() && !cpp.close());
    let range = &rows[4];
    assert_eq!((range.unit, range.reference), ("st", 24.0));
    assert_eq!(range.delta_text(), "+0.0 st");
    assert_eq!(rows[3].tolerance, 3.0);
    assert_eq!(rows[5].tolerance, 4.0);

    // A take with no range (no pitch) drops the row.
    let mut silent = canned_analysis();
    let mut profile = (*silent.profile).clone();
    profile.low_midi = None;
    silent.profile = Arc::new(profile);
    let labels: Vec<&str> = gap_rows(&reference, &silent)
        .iter()
        .map(|r| r.label)
        .collect();
    assert!(!labels.contains(&"RANGE"));
}

// `_QualityWarning`: a clipped take, then a noisy one, flagged rather than
// silently mis-analyzed.
#[test]
fn a_clipped_or_noisy_take_is_flagged() {
    let clean = canned_analysis();
    assert_eq!(quality_warning(&clean.metrics), None);
    let clipped = analysis_with(|m| m.clipped_fraction = 0.0125);
    assert_eq!(
        quality_warning(&clipped.metrics).as_deref(),
        Some(
            "Clipped (1.3% of samples). Lower the mic gain and re-record — the spectral \
             metrics are unreliable on a clipped take."
        )
    );
    let noisy = analysis_with(|m| m.snr_db = 7.5);
    assert_eq!(
        quality_warning(&noisy.metrics).as_deref(),
        Some(
            "Low signal-to-noise (8 dB). Record closer to the mic, or somewhere quieter, \
             for a trustworthy analysis."
        )
    );
}

#[test]
fn the_level_meter_fills_to_the_level() {
    assert_eq!(meter_fill(0.5, 90.0), 45.0);
    assert_eq!(meter_fill(1.5, 90.0), 90.0);
    assert_eq!(meter_fill(-1.0, 90.0), 0.0);
    assert_eq!(meter_fill(f32::NAN, 90.0), 0.0);
}
