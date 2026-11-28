//! A song-practice card on the library grid (port of
//! `ui/home/widgets/practice_project_card.dart`, drawn as `DESIGN.md`'s
//! library card and the approved `Home.dc.html` mockup). Tapping opens the
//! workspace; the trash button, shown while the card is hovered, requests
//! deletion (confirmation handled by the caller).
//!
//! The card: radius 8, `#18181C` with an inset `Studio::BORDER` outline; a
//! 76 px waveform band (`Studio::WELL`) over a 1 px line, holding the
//! practice's waveform in its colour (or "No recordings yet" when it has no
//! take and no backing) and its length at the bottom right; then the title
//! (13 px semibold, one line, ellipsized) and a meta row ("Backing · 3 takes
//! · 4 notes" in `TEXT_SECONDARY`, the relative date in `TEXT_FAINT`).
//! Under the pointer it lifts: `#1C1C21` with a `#3A3A43` outline and a soft
//! shadow, over 120 ms, linear (the Dart card's `AnimatedContainer`
//! timing). Hover is iced's `hover`: the trash draws and takes input only
//! while the pointer is over the card.
//!
//! The waveform comes from the library's waveform cache
//! ([`PracticePreview`], read off the UI thread by the screen); until it
//! has come, or when the practice has no cached peaks, the band is empty.

use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{button, canvas as canvas_widget, column, container, hover, row, stack, Space};
use iced::{
    mouse, Color, Element, Length, Padding, Point, Rectangle, Renderer, Shadow, Size, Theme, Vector,
};
use vocara_model::models::practice_summary::{PracticePreview, PracticeSummary, PreviewSource};
use vocara_model::util::dart_date_time::DateTime;

use crate::ui::core::icons::Icon;
use crate::ui::core::theme::{alpha, bare_button, boxed, font, type_scale, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};
use crate::ui::core::widgets::controls::icon_button;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::song_practice_detail::widgets::track_row::lane_color;

/// How long the card takes to lift (`AnimatedContainer`).
pub const ANIMATION: Duration = Duration::from_millis(120);

/// The card's height: the band, its line, and the title and meta rows in
/// their padding (10 + 18 + 4 + 16 + 12).
pub const HEIGHT: f32 = BAND_HEIGHT + 1.0 + 60.0;

/// The waveform band's height.
pub const BAND_HEIGHT: f32 = 76.0;

/// The card's fill at rest and lifted, and its lifted outline.
pub const FILL: Color = Color::from_rgb8(0x18, 0x18, 0x1C);
pub const FILL_HOVERED: Color = Color::from_rgb8(0x1C, 0x1C, 0x21);
pub const OUTLINE_HOVERED: Color = Color::from_rgb8(0x3A, 0x3A, 0x43);

/// The line under the band.
const BAND_LINE: Color = Studio::HOVER;

/// The card's look at rest or lifted (under the pointer).
pub fn card_look(hovered: bool) -> Decoration {
    if hovered {
        Decoration::boxed(FILL_HOVERED, 8.0, OUTLINE_HOVERED, 1.0).with_shadow(Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 16.0,
        })
    } else {
        Decoration::boxed(FILL, 8.0, Studio::BORDER, 1.0)
    }
}

/// "Backing · 3 takes · 4 notes": the backing, the takes, the notes (a
/// count of none left out), the first word capitalized. With nothing at
/// all, "No takes yet".
pub fn meta(p: &PracticeSummary) -> String {
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    if !p.has_accompaniment && p.take_count == 0 && p.note_count == 0 {
        return "No takes yet".to_string();
    }
    let mut parts = Vec::new();
    if p.has_accompaniment {
        parts.push("Backing".to_string());
    }
    parts.push(if p.take_count == 0 {
        "no takes".to_string()
    } else {
        plural(p.take_count, "take", "takes")
    });
    if p.note_count > 0 {
        parts.push(plural(p.note_count, "note", "notes"));
    }
    let joined = parts.join(" · ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => joined,
    }
}

/// When `date` was, said relative to `now` (both in local time): "Today",
/// "Yesterday", "Nov 24", or "Nov 24, 2025" in another year.
pub fn relative_date(date: &DateTime, now: &DateTime) -> String {
    let (date, now) = (date.to_local(), now.to_local());
    let ymd = |d: &DateTime| (d.year(), d.month(), d.day());
    if ymd(&date) == ymd(&now) {
        return "Today".to_string();
    }
    let yesterday = DateTime::local(now.year(), now.month(), now.day() - 1, 0, 0, 0, 0, 0);
    if yesterday.is_some_and(|y| ymd(&y) == ymd(&date)) {
        return "Yesterday".to_string();
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[(date.month().clamp(1, 12) - 1) as usize];
    if date.year() == now.year() {
        format!("{month} {}", date.day())
    } else {
        format!("{month} {}, {}", date.day(), date.year())
    }
}

/// A length as the band reads it: "0:48", "4:10", "1:02:03".
pub fn length_label(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The colour a practice's waveform takes: its track's on the practice page
/// (`track_row::lane_color`: its tint, the backing's neutral, else the
/// take's own by its id). It was a colour by the practice's id, which
/// matched no take's.
pub fn waveform_color(preview: &PracticePreview, source: PreviewSource) -> Color {
    lane_color(
        &preview.track_id,
        preview.color_value,
        source == PreviewSource::Backing,
    )
}

/// `PracticeProjectCard(practice: ..., onOpen: ..., onDelete: ...)`, with
/// its `preview` once it has come, dated relative to `now`.
pub fn practice_project_card<'a, M: Clone + 'a>(
    practice: &PracticeSummary,
    preview: Option<&PracticePreview>,
    now: &DateTime,
    on_open: M,
    on_delete: M,
) -> Element<'a, M> {
    let body = column![
        band(practice, preview),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(|_| boxed(Some(BAND_LINE), 0.0, None)),
        container(
            column![
                studio_text(practice.title.clone())
                    .size(type_scale::TITLE)
                    .font(font::SEMIBOLD)
                    .line_height(iced::Pixels(18.0))
                    .color(Studio::TEXT_PRIMARY)
                    .ellipsis(1),
                row![
                    studio_text(meta(practice))
                        .size(type_scale::LABEL)
                        .line_height(iced::Pixels(type_scale::LINE))
                        .tabular()
                        .color(Studio::TEXT_SECONDARY)
                        .width(Length::Fill)
                        .ellipsis(1),
                    studio_text(relative_date(&practice.updated_at, now))
                        .size(type_scale::LABEL)
                        .line_height(iced::Pixels(type_scale::LINE))
                        .color(Studio::TEXT_FAINT),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            ]
            .spacing(4),
        )
        .padding(Padding {
            top: 10.0,
            right: 12.0,
            bottom: 12.0,
            left: 12.0,
        }),
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    let tap = button(body)
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press(on_open)
        .style(bare_button);
    // The outline over the band's corners: drawn above the content.
    let card = animated_container(tap, ANIMATION)
        .border_above()
        .style(card_look);
    // The trash at the band's top right, over the waveform.
    let trash = container(
        icon_button(Icon::Trash2, on_delete)
            .panel()
            .tooltip("Delete practice"),
    )
    .padding(6)
    .width(Length::Fill)
    .align_x(Horizontal::Right);
    hover(card, trash)
}

/// The waveform band: the practice's waveform (or "No recordings yet") and
/// its length, on the band's well, its top corners the card's.
fn band<'a, M: 'a>(
    practice: &PracticeSummary,
    preview: Option<&PracticePreview>,
) -> Element<'a, M> {
    let well = container(Space::new())
        .width(Length::Fill)
        .height(BAND_HEIGHT)
        .style(|_| iced::widget::container::Style {
            border: iced::Border {
                radius: Radius {
                    top_left: 8.0,
                    top_right: 8.0,
                    bottom_right: 0.0,
                    bottom_left: 0.0,
                },
                ..iced::Border::default()
            },
            ..boxed(Some(Studio::WELL), 0.0, None)
        });
    let mut layers = stack![well].width(Length::Fill).height(BAND_HEIGHT);
    let recorded = practice.take_count > 0 || practice.has_accompaniment;
    if !recorded {
        layers = layers.push(
            container(
                studio_text("No recordings yet")
                    .size(type_scale::LABEL)
                    .color(Studio::TEXT_DISABLED),
            )
            .center(Length::Fill),
        );
    } else if let Some((p, source)) = preview.and_then(|p| p.source.map(|s| (p, s))) {
        let color = alpha(waveform_color(p, source), 0.85);
        layers = layers.push(
            container(
                canvas_widget(Waveform {
                    peaks: p.peaks.clone(),
                    color,
                })
                .width(Length::Fill)
                .height(60),
            )
            .padding(Padding {
                top: 8.0,
                right: 12.0,
                bottom: 8.0,
                left: 12.0,
            }),
        );
    }
    if let Some(length) = preview.map(|p| p.length_ms).filter(|ms| *ms > 0) {
        layers = layers.push(
            container(
                studio_text(length_label(length))
                    .size(type_scale::AXIS)
                    .line_height(iced::Pixels(12.0))
                    .tabular()
                    .color(Studio::TEXT_FAINT),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding {
                top: 0.0,
                right: 8.0,
                bottom: 6.0,
                left: 0.0,
            })
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom),
        );
    }
    layers.into()
}

/// The band's waveform: a filled, symmetric peak envelope over a faint
/// centre line, each level scaled to the loudest (which reaches 26 px
/// either side of the middle of the 60 px area), never thinner than 0.4.
struct Waveform {
    peaks: Vec<f32>,
    color: Color,
}

/// The envelope's half-height at full level, and its least.
const ENVELOPE: f32 = 26.0;
const LEAST: f32 = 0.4;

impl<M> canvas::Program<M> for Waveform {
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
        let mid = (bounds.height / 2.0).round();
        frame.fill_rectangle(
            Point::new(0.0, mid - 0.5),
            Size::new(bounds.width, 1.0),
            Studio::BORDER_SUBTLE,
        );
        let loudest = self.peaks.iter().copied().fold(0.0f32, f32::max);
        if self.peaks.len() >= 2 && loudest > 0.0 {
            let step = bounds.width / (self.peaks.len() - 1) as f32;
            let half = |v: f32| (v / loudest * ENVELOPE).max(LEAST);
            let envelope = Path::new(|b| {
                for (i, v) in self.peaks.iter().enumerate() {
                    let p = Point::new(i as f32 * step, mid - half(*v));
                    if i == 0 {
                        b.move_to(p);
                    } else {
                        b.line_to(p);
                    }
                }
                for (i, v) in self.peaks.iter().enumerate().rev() {
                    b.line_to(Point::new(i as f32 * step, mid + half(*v)));
                }
                b.close();
            });
            frame.fill(&envelope, self.color);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::icons;
    use std::borrow::Cow;

    /// The app's fonts (Inter, and the Lucide icons). A simulator loads its
    /// fonts into the font system every test shares, so a test drawing with
    /// none of its own drew with whatever other tests had loaded by then: the
    /// same card drew differently before and after another test loaded Inter.
    fn settings() -> iced::Settings {
        let mut settings = crate::app::settings();
        settings.fonts = crate::ui::core::theme::font::FACES
            .iter()
            .copied()
            .chain([icons::FONT_BYTES])
            .map(Cow::Borrowed)
            .collect();
        settings
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Open,
        Delete,
    }

    fn summary(takes: usize, notes: usize, backing: bool) -> PracticeSummary {
        PracticeSummary {
            id: "P1".into(),
            title: "Bohemian Rhapsody".into(),
            updated_at: DateTime::local(2026, 3, 4, 5, 6, 7, 0, 0).expect("a date"),
            take_count: takes,
            has_accompaniment: backing,
            note_count: notes,
        }
    }

    fn now() -> DateTime {
        DateTime::local(2026, 3, 4, 18, 0, 0, 0, 0).expect("a date")
    }

    fn preview(id: &str) -> PracticePreview {
        PracticePreview {
            id: id.into(),
            length_ms: 48_000,
            source: Some(PreviewSource::Take),
            track_id: format!("{id}-take"),
            color_value: None,
            peaks: (0..PracticePreview::BUCKETS)
                .map(|i| ((i as f32) / 20.0).sin().abs())
                .collect(),
        }
    }

    fn card(p: &PracticeSummary, preview: Option<&PracticePreview>) -> Element<'static, Msg> {
        container(practice_project_card(
            p,
            preview,
            &now(),
            Msg::Open,
            Msg::Delete,
        ))
        .width(262)
        .height(HEIGHT)
        .into()
    }

    #[test]
    fn the_meta_row_reads_backing_takes_and_notes() {
        assert_eq!(meta(&summary(3, 4, true)), "Backing · 3 takes · 4 notes");
        assert_eq!(meta(&summary(1, 0, true)), "Backing · 1 take");
        assert_eq!(meta(&summary(0, 0, true)), "Backing · no takes");
        assert_eq!(meta(&summary(6, 1, false)), "6 takes · 1 note");
        assert_eq!(meta(&summary(0, 0, false)), "No takes yet");
        assert_eq!(meta(&summary(0, 2, false)), "No takes · 2 notes");
    }

    #[test]
    fn the_date_reads_relative_to_today() {
        let at = |y, m, d| DateTime::local(y, m, d, 9, 30, 0, 0, 0).expect("a date");
        let now = at(2026, 11, 27);
        assert_eq!(relative_date(&at(2026, 11, 27), &now), "Today");
        assert_eq!(relative_date(&at(2026, 11, 26), &now), "Yesterday");
        assert_eq!(relative_date(&at(2026, 11, 24), &now), "Nov 24");
        assert_eq!(relative_date(&at(2025, 11, 24), &now), "Nov 24, 2025");
        // Across a month and a year.
        assert_eq!(
            relative_date(&at(2026, 2, 28), &at(2026, 3, 1)),
            "Yesterday"
        );
        assert_eq!(
            relative_date(&at(2025, 12, 31), &at(2026, 1, 1)),
            "Yesterday"
        );
        assert_eq!(
            relative_date(&at(2025, 12, 30), &at(2026, 1, 1)),
            "Dec 30, 2025"
        );
    }

    #[test]
    fn the_length_reads_minutes_and_seconds() {
        assert_eq!(length_label(48_000), "0:48");
        assert_eq!(length_label(250_400), "4:10");
        assert_eq!(length_label(3_723_000), "1:02:03");
        assert_eq!(length_label(-5), "0:00");
    }

    // The card's waveform took a colour by the practice's id, which matched
    // no take's: the take it draws reads in its own colour on the page.
    #[test]
    fn the_waveform_is_drawn_in_its_takes_colour_and_a_backing_is_neutral() {
        use crate::ui::song_practice_detail::widgets::track_context_menu::argb;
        use crate::ui::song_practice_detail::widgets::track_row::take_color;
        use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
        let wave = preview("P1");
        let take = AudioTrack::new("P1-take", AudioTrackKind::Vocal, "Take 1", now());
        assert_eq!(waveform_color(&wave, PreviewSource::Take), take_color(&take));
        let tinted = wave.clone().of_track("P1-take", Some(0xFF5B8DEF));
        assert_eq!(
            waveform_color(&tinted, PreviewSource::Take),
            argb(0xFF5B8DEF)
        );
        assert_eq!(
            waveform_color(&wave, PreviewSource::Backing),
            Studio::TRACK_BACKING
        );
    }

    #[test]
    fn the_card_shows_the_practice() {
        let p = summary(2, 1, true);
        let mut ui = iced_test::simulator(card(&p, Some(&preview("P1"))));
        assert!(ui.find("Bohemian Rhapsody").is_ok());
        assert!(ui.find("Backing · 2 takes · 1 note").is_ok());
        assert!(ui.find("Today").is_ok());
        assert!(ui.find("0:48").is_ok());
        // Before its preview: no length, no "No recordings yet".
        let mut ui = iced_test::simulator(card(&p, None));
        assert!(ui.find("0:48").is_err());
        assert!(ui.find("No recordings yet").is_err());
        // Nothing recorded: the band says so.
        let mut ui = iced_test::simulator(card(&summary(0, 0, false), None));
        assert!(ui.find("No recordings yet").is_ok());
        assert!(ui.find("No takes yet").is_ok());
    }

    #[test]
    fn a_long_title_ends_in_an_ellipsis_within_the_card() {
        use iced_test::selector::{Candidate, Target};

        let long = "A practice whose title runs on far past the width of its card";
        let p = PracticeSummary {
            title: long.into(),
            ..summary(2, 1, true)
        };
        let mut ui = iced_test::Simulator::with_settings(settings(), card(&p, None));
        // The title is still found by its full text...
        let title = ui.find(long).expect("the title");
        // ...and what shows is its longest prefix that fits, then "…",
        // inside the card's 12 px of padding.
        let shown = ui
            .find(|candidate: Candidate<'_>| match candidate {
                Candidate::Text { content, .. } if content.ends_with('\u{2026}') => {
                    Some((content.to_string(), Target::from(candidate)))
                }
                _ => None,
            })
            .expect("the cut title");
        let prefix = shown.0.trim_end_matches('\u{2026}');
        assert!(long.starts_with(prefix) && prefix.len() > 10, "{}", shown.0);
        assert!(title.bounds().width <= 262.0 - 2.0 * 12.0);
        // A title that fits shows whole.
        let mut ui =
            iced_test::Simulator::with_settings(settings(), card(&summary(2, 1, true), None));
        assert!(ui
            .find(|candidate: Candidate<'_>| match candidate {
                Candidate::Text { content, .. }
                    if content.ends_with('\u{2026}') && content.starts_with("Bohemian") =>
                    Some(()),
                _ => None,
            })
            .is_err());
    }

    #[test]
    fn a_tap_opens_the_practice_and_the_trash_deletes_it() {
        let p = summary(2, 1, true);
        let mut ui = iced_test::simulator(card(&p, None));
        ui.click("Bohemian Rhapsody").expect("the title");
        let trash = char::from(Icon::Trash2).to_string();
        ui.click(trash.as_str()).expect("the trash");
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Open, Msg::Delete]
        );
    }

    #[test]
    fn the_card_lifts_over_120_ms_under_the_pointer() {
        use crate::ui::core::motion::Lerp;
        use crate::ui::core::widgets::animated::testing::{near, Frames, MS};
        use iced::time::Instant;

        let p = summary(2, 1, true);
        let mut window = Frames::new(Size::new(262.0, HEIGHT));
        // Inside the card, below the band, clear of the text: its fill.
        let fill = Point::new(4.0, HEIGHT - 4.0);
        // The left outline, halfway down the band.
        let edge = Point::new(0.0, 40.0);
        let t0 = Instant::now();
        window.frame(card(&p, None), &[], t0);
        assert!(near(window.pixel(fill), FILL), "{:?}", window.pixel(fill));
        assert!(
            near(window.pixel(edge), Studio::BORDER),
            "{:?}",
            window.pixel(edge)
        );
        window.point_at(Point::new(131.0, 120.0));
        assert!(window.frame(card(&p, None), &[], t0));
        assert!(near(window.pixel(fill), FILL), "start");
        window.frame(card(&p, None), &[], t0 + 60 * MS);
        assert!(
            near(window.pixel(fill), FILL.lerp(FILL_HOVERED, 0.5)),
            "middle: {:?}",
            window.pixel(fill)
        );
        assert!(!window.frame(card(&p, None), &[], t0 + 120 * MS));
        assert!(near(window.pixel(fill), FILL_HOVERED), "end");
        assert!(near(window.pixel(edge), OUTLINE_HOVERED), "outline");
    }

    // The outlines of the cards scrolled under Home's toolbar were drawn
    // over it: the outline's layer was clipped to the card, not to the list
    // it scrolls in.
    #[test]
    fn a_card_scrolled_under_the_toolbar_draws_no_outline_over_it() {
        use crate::ui::core::widgets::animated::testing::{near, Frames, BACKGROUND};
        use crate::ui::core::widgets::fading_scrollbar::scrollable;
        use iced::time::Instant;
        use iced::{mouse, Event};

        const BAR: Color = Color::from_rgb8(0x40, 0x10, 0x10);
        let p = summary(2, 1, true);
        // A 50 px toolbar over a list whose card starts 20 px down it.
        let view = || -> Element<'static, Msg> {
            iced::widget::column![
                container(Space::new())
                    .width(Length::Fill)
                    .height(50)
                    .style(|_| boxed(Some(BAR), 0.0, None)),
                scrollable(iced::widget::column![
                    Space::new().height(20),
                    container(card(&p, None)).padding([0, 20]),
                    Space::new().height(1000),
                ])
                .width(Length::Fill)
                .height(Length::Fill),
            ]
            .into()
        };
        let mut window = Frames::new(Size::new(400.0, 400.0));
        // Beside the card, so it is not hovered.
        window.point_at(Point::new(350.0, 300.0));
        window.frame(view(), &[], Instant::now());
        let band = Point::new(100.0, 60.0);
        assert!(near(window.pixel(band), BACKGROUND), "the card below it");
        // Scrolled 60 px: the card's top 40 px are under the toolbar.
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -60.0 },
        });
        let t0 = Instant::now();
        window.frame(view(), &[wheel], t0);
        window.frame(view(), &[], t0 + Duration::from_secs(1));
        assert!(!near(window.pixel(band), BACKGROUND), "scrolled up to it");
        // The card's left edge, under the toolbar and in the list.
        assert!(near(window.pixel(Point::new(20.0, 30.0)), BAR), "over the bar");
        assert!(near(window.pixel(Point::new(20.0, 100.0)), Studio::BORDER));
    }

    #[test]
    fn the_band_draws_the_waveform_in_the_practice_s_colour() {
        use crate::ui::core::widgets::animated::testing::{near, over, Frames};
        use iced::time::Instant;

        let p = summary(2, 1, true);
        let wave = preview("P1");
        let mut window = Frames::new(Size::new(262.0, HEIGHT));
        window.frame(card(&p, Some(&wave)), &[], Instant::now());
        // A row 8 px above the band's middle (8 + 30): the envelope's
        // colour wherever the level reaches it; at the band's corner, the
        // well.
        let colour = over(
            alpha(waveform_color(&wave, PreviewSource::Take), 0.85),
            Studio::WELL,
        );
        let lit = (14..248)
            .filter(|x| near(window.pixel(Point::new(*x as f32, 30.0)), colour))
            .count();
        assert!(lit > 50, "{lit} columns in the waveform's colour");
        assert!(near(window.pixel(Point::new(4.0, 4.0)), Studio::WELL));
        // Without a preview, the band is the well alone.
        let mut window = Frames::new(Size::new(262.0, HEIGHT));
        window.frame(card(&p, None), &[], Instant::now());
        assert!(near(window.pixel(Point::new(100.0, 30.0)), Studio::WELL));
    }

    #[test]
    fn hovering_a_card_reveals_its_trash() {
        let p = summary(2, 1, true);
        let snapshot = |cursor: Option<Point>| {
            let mut ui = iced_test::Simulator::with_size(
                settings(),
                Size::new(262.0, HEIGHT),
                card(&p, None),
            );
            if let Some(at) = cursor {
                ui.point_at(at);
            }
            ui.snapshot(&crate::ui::core::theme::app_theme())
                .expect("snapshot")
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("card.png");
        assert!(snapshot(None).matches_image(&path).expect("write"));
        // Hovered, it draws differently (the trash, the lifted card)...
        assert!(!snapshot(Some(Point::new(131.0, 120.0)))
            .matches_image(&path)
            .expect("compare"));
        // ...and away from it, as at rest.
        assert!(snapshot(Some(Point::new(500.0, 500.0)))
            .matches_image(&path)
            .expect("compare"));
    }
}
