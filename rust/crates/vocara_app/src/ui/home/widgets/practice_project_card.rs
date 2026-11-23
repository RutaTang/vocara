//! Port of `ui/home/widgets/practice_project_card.dart`: a song-practice
//! tile on the home grid. Tapping opens the workspace; the trash button,
//! shown while the card is hovered, requests deletion (confirmation handled
//! by the caller).
//!
//! The card fills the grid cell it is given (Dart's grid made each 150 tall,
//! at most 280 wide). Hover is iced's `hover`: the trash layer draws and
//! takes input only while the pointer is over the card, as Dart's
//! `MouseRegion` built it only then, and the card's own border turns coral
//! from its button status. Flutter animated the border over 120 ms
//! (`AnimatedContainer`); iced switches at once. The title is held to one
//! line (Dart ellipsized it; iced clips it).

use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{button, column, container, hover, row, text, Space};
use iced::{Background, Border, Color, Element, Length, Shadow, Theme};
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::util::dart_date_time::DateTime;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, boxed, font, Studio};

/// The card's border: coral at 60 % while hovered, `Studio.border` at rest.
pub fn card_border(hovered: bool) -> Color {
    if hovered {
        alpha(Studio::ACCENT, 0.6)
    } else {
        Studio::BORDER
    }
}

/// `backing · 2 takes · 1 note`: the backing marker, then the take and
/// note counts.
pub fn subtitle(p: &PracticeSummary) -> String {
    let takes = p.take_count;
    let take_label = if takes == 1 {
        "1 take".to_string()
    } else {
        format!("{takes} takes")
    };
    let notes = p.note_count;
    let note_label = if notes == 1 {
        "1 note".to_string()
    } else {
        format!("{notes} notes")
    };
    let accompaniment = if p.has_accompaniment {
        "backing · "
    } else {
        ""
    };
    format!("{accompaniment}{take_label} · {note_label}")
}

/// `2026-03-04 05:06`, in the date's own zone.
pub fn format_date(date: &DateTime) -> String {
    format!(
        "{}-{:02}-{:02} {:02}:{:02}",
        date.year(),
        date.month(),
        date.day(),
        date.hour(),
        date.minute()
    )
}

/// `PracticeProjectCard(practice: ..., onOpen: ..., onDelete: ...)`.
pub fn practice_project_card<'a, M: Clone + 'a>(
    practice: &PracticeSummary,
    on_open: M,
    on_delete: M,
) -> Element<'a, M> {
    let badge = container(icons::icon(Icon::Music, 18.0, Studio::ACCENT))
        .padding(8)
        .style(|_| boxed(Some(alpha(Studio::ACCENT, 0.15)), 8.0, None));
    let content = column![
        row![badge, Space::new().width(Length::Fill)],
        Space::new().height(Length::Fill),
        container(
            text(practice.title.clone())
                .size(16)
                .font(font::SEMIBOLD)
                .color(Studio::TEXT_PRIMARY)
                .wrapping(Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
        Space::new().height(4),
        text(subtitle(practice))
            .size(12)
            .color(Studio::TEXT_SECONDARY),
        Space::new().height(2),
        text(format!("Updated {}", format_date(&practice.updated_at)))
            .size(11)
            .color(Studio::TEXT_FAINT),
    ]
    .width(Length::Fill)
    .height(Length::Fill);
    // 16 of padding inside the 1 px border (a decoration's border pads its
    // child; an iced border does not).
    let card = button(container(content).padding(17))
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .on_press(on_open)
        .style(|_: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(Studio::PANEL)),
                text_color: Studio::TEXT_PRIMARY,
                border: Border {
                    color: card_border(hovered),
                    width: 1.0,
                    radius: 10.0.into(),
                },
                shadow: Shadow::default(),
                snap: false,
            }
        });
    // The trash sits at the end of the badge's row, centred on the 34 px
    // badge; its own 16 px glyph is the hit area.
    let trash = button(icons::icon(Icon::Trash2, 16.0, Studio::TEXT_FAINT))
        .padding(0)
        .style(bare_button)
        .on_press(on_delete);
    let hovered_layer = container(
        row![Space::new().width(Length::Fill), trash]
            .height(34)
            .align_y(Vertical::Center),
    )
    .padding(17)
    .width(Length::Fill)
    .height(Length::Fill);
    hover(card, hovered_layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, Settings, Size};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Open,
        Delete,
    }

    fn summary(takes: usize, notes: usize, backing: bool) -> PracticeSummary {
        PracticeSummary {
            id: "P1".into(),
            title: "Bohemian Rhapsody".into(),
            updated_at: DateTime::utc(2026, 3, 4, 5, 6, 7, 0, 0).expect("a date"),
            take_count: takes,
            has_accompaniment: backing,
            note_count: notes,
        }
    }

    fn card(p: &PracticeSummary) -> Element<'static, Msg> {
        container(practice_project_card(p, Msg::Open, Msg::Delete))
            .width(280)
            .height(150)
            .into()
    }

    #[test]
    fn the_subtitle_counts_takes_and_notes() {
        assert_eq!(subtitle(&summary(1, 1, false)), "1 take · 1 note");
        assert_eq!(subtitle(&summary(0, 2, false)), "0 takes · 2 notes");
        assert_eq!(
            subtitle(&summary(3, 0, true)),
            "backing · 3 takes · 0 notes"
        );
    }

    #[test]
    fn the_date_reads_year_first_with_zero_padding() {
        assert_eq!(
            format_date(&DateTime::utc(2026, 3, 4, 5, 6, 7, 0, 0).expect("a date")),
            "2026-03-04 05:06"
        );
        assert_eq!(
            format_date(&DateTime::utc(2025, 12, 31, 23, 59, 0, 0, 0).expect("a date")),
            "2025-12-31 23:59"
        );
    }

    #[test]
    fn the_card_shows_the_practice() {
        let p = summary(2, 1, true);
        let mut ui = iced_test::simulator(card(&p));
        assert!(ui.find("Bohemian Rhapsody").is_ok());
        assert!(ui.find("backing · 2 takes · 1 note").is_ok());
        assert!(ui.find("Updated 2026-03-04 05:06").is_ok());
    }

    #[test]
    fn a_tap_opens_the_practice_and_the_trash_deletes_it() {
        let p = summary(2, 1, true);
        let mut ui = iced_test::simulator(card(&p));
        ui.click("Bohemian Rhapsody").expect("the title");
        let trash = char::from(Icon::Trash2).to_string();
        ui.click(trash.as_str()).expect("the trash");
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Open, Msg::Delete]
        );
    }

    #[test]
    fn hovering_a_card_reveals_its_trash_and_the_coral_border() {
        assert_eq!(card_border(false), Studio::BORDER);
        assert!((card_border(true).a - 0.6).abs() < 1e-6);
        let p = summary(2, 1, true);
        let snapshot = |cursor: Option<Point>| {
            let mut ui = iced_test::Simulator::with_size(
                Settings::default(),
                Size::new(280.0, 150.0),
                card(&p),
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
        // Hovered, it draws differently (the trash and the coral border)...
        assert!(!snapshot(Some(Point::new(140.0, 120.0)))
            .matches_image(&path)
            .expect("compare"));
        // ...and away from it, as at rest.
        assert!(snapshot(Some(Point::new(500.0, 500.0)))
            .matches_image(&path)
            .expect("compare"));
    }
}
