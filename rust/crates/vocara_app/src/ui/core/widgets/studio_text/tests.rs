use super::testing::{laid, spec};
use super::*;
use crate::ui::core::theme::font;

fn width_of(spec: &Spec<'_>) -> f32 {
    laid(spec, f32::INFINITY).min_bounds.width
}

/// `content` as an ellipsized text measures it once cut: shaped fully (the
/// "…" is not ASCII), unspaced.
fn solid_width(content: &str, size: f32) -> f32 {
    width_of(&Spec {
        shaping: Shaping::Advanced,
        ..spec(content, size, font::SEMIBOLD)
    })
}

fn ellipsized<'s>(content: &'s str, size: f32, max_lines: usize) -> Spec<'s> {
    Spec {
        max_lines: Some(max_lines),
        ..spec(content, size, font::SEMIBOLD)
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn spacing_follows_every_glyph_the_last_one_too() {
    // SkParagraph adds the spacing after each glyph, trailing one included.
    for (content, glyphs) in [("PITCH RANGE", 11.0), ("A", 1.0), ("Vocara", 6.0)] {
        let solid = spec(content, 9.0, font::BOLD);
        let spaced = Spec {
            letter_spacing: 0.5,
            ..solid.clone()
        };
        let grown = width_of(&spaced) - width_of(&solid);
        assert!(
            close(grown, 0.5 * glyphs),
            "{content}: grew {grown}, not {}",
            0.5 * glyphs
        );
    }
}

#[test]
fn spacing_keeps_the_shaping_of_non_latin_text() {
    // Cyrillic (Inter has it) is shaped fully under `Auto`, and spaced per
    // glyph as Latin is.
    let solid = spec("Привет", 12.0, font::REGULAR);
    let spaced = Spec {
        letter_spacing: 1.0,
        ..solid.clone()
    };
    let grown = width_of(&spaced) - width_of(&solid);
    assert!(close(grown, 6.0), "grew {grown}");
    assert!(width_of(&solid) > 0.0);
}

#[test]
fn spacing_keeps_the_line_height() {
    let solid = laid(&spec("CAP", 9.0, font::BOLD), f32::INFINITY);
    let spaced = laid(
        &Spec {
            letter_spacing: 0.8,
            line_height: 9.0,
            ..spec("CAP", 9.0, font::BOLD)
        },
        f32::INFINITY,
    );
    assert!(close(solid.min_bounds.height, 9.0 * 1.3));
    assert!(close(spaced.min_bounds.height, 9.0));
}

#[test]
fn a_text_that_fits_has_no_ellipsis() {
    // A text that fits is laid out exactly as iced's text would be.
    let title = "Moon River";
    let natural = width_of(&spec(title, 16.0, font::SEMIBOLD));
    let fitted = laid(&ellipsized(title, 16.0, 1), natural + 0.5);
    assert_eq!(fitted.shown, None);
    assert!(close(fitted.min_bounds.width, natural));
    // Exactly as wide as the room still fits.
    let exact = laid(&ellipsized(title, 16.0, 1), natural);
    assert_eq!(exact.shown, None);
    // So does a non-Latin one.
    let title = "Лунная река";
    let natural = width_of(&spec(title, 16.0, font::SEMIBOLD));
    assert_eq!(laid(&ellipsized(title, 16.0, 1), natural).shown, None);
    let cut = laid(&ellipsized(title, 16.0, 1), natural - 1.0);
    let shown = cut.shown.expect("cut");
    assert!(title.starts_with(shown.trim_end_matches(ELLIPSIS)));
    assert!(cut.min_bounds.width <= natural - 1.0 + 0.01);
}

#[test]
fn a_long_line_keeps_the_longest_prefix_that_fits_with_the_ellipsis() {
    let title = "A very long practice title that will not fit";
    for room in [60.0, 97.5, 140.0, 201.0] {
        let cut = laid(&ellipsized(title, 16.0, 1), room);
        let shown = cut.shown.expect("cut");
        let prefix = shown.strip_suffix(ELLIPSIS).expect("ends in …");
        assert!(title.starts_with(prefix), "{shown}");
        assert!(
            solid_width(&shown, 16.0) <= room + 0.01,
            "{shown} overflows {room}"
        );
        let next = title[prefix.len()..].chars().next().expect("text left");
        let longer = format!("{prefix}{next}{ELLIPSIS}");
        assert!(
            solid_width(&longer, 16.0) > room,
            "{longer} fits {room} too"
        );
        assert!(cut.min_bounds.width <= room + 0.01);
        assert!(close(cut.min_bounds.height, 16.0 * 1.3));
    }
}

#[test]
fn the_ellipsis_is_set_without_the_spacing() {
    let label = "SEPARATED VOCALS";
    let spaced = Spec {
        letter_spacing: 0.8,
        ..ellipsized(label, 10.0, 1)
    };
    let cut = laid(&spaced, 70.0);
    let shown = cut.shown.expect("cut");
    let prefix = shown.strip_suffix(ELLIPSIS).expect("ends in …");
    let prefix_width = width_of(&Spec {
        letter_spacing: 0.8,
        shaping: Shaping::Advanced,
        ..spec(prefix, 10.0, font::SEMIBOLD)
    });
    let ellipsis_width = width_of(&Spec {
        shaping: Shaping::Advanced,
        ..spec(ELLIPSIS, 10.0, font::SEMIBOLD)
    });
    assert!(
        (cut.min_bounds.width - (prefix_width + ellipsis_width)).abs() < 0.5,
        "{} vs {} + {}",
        cut.min_bounds.width,
        prefix_width,
        ellipsis_width
    );
    assert!(cut.min_bounds.width <= 70.01);
}

#[test]
fn a_hard_break_past_the_last_line_ends_it_in_an_ellipsis() {
    let cut = laid(&ellipsized("verse\nchorus", 13.0, 1), 300.0);
    assert_eq!(cut.shown.as_deref(), Some("verse\u{2026}"));
}

#[test]
fn max_lines_wraps_then_ellipsizes_the_last_line() {
    let text = "Delete the take and every clip recorded into it from this practice";
    let room = 150.0;
    let cut = laid(&ellipsized(text, 13.0, 2), room);
    let shown = cut.shown.expect("cut");
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 2, "{shown:?}");
    // The first line is the wrapped one, whole words.
    assert!(text.starts_with(lines[0]));
    assert!(text[lines[0].len()..].starts_with(' '));
    // The second runs on from it, cut to fit with the "…".
    let rest = text[lines[0].len()..].trim_start();
    let prefix = lines[1].strip_suffix(ELLIPSIS).expect("ends in …");
    assert!(rest.starts_with(prefix), "{prefix:?}");
    assert!(solid_width(lines[1], 13.0) <= room + 0.01);
    assert!(close(cut.min_bounds.height, 2.0 * 13.0 * 1.3));
    // Room for all of it: no ellipsis, the lines it needs.
    let whole = laid(&ellipsized(text, 13.0, 2), 1000.0);
    assert_eq!(whole.shown, None);
    assert!(close(whole.min_bounds.height, 13.0 * 1.3));
}

#[test]
fn an_unbounded_width_never_cuts() {
    let long = "x".repeat(400);
    assert_eq!(laid(&ellipsized(&long, 12.0, 1), f32::INFINITY).shown, None);
}

#[test]
fn a_canvas_label_puts_each_cluster_where_the_spaced_line_has_it() {
    super::testing::load_fonts();
    let solid = spec("INSPECTOR", 11.0, font::BOLD);
    let spaced = Spec {
        letter_spacing: 1.5,
        ..solid.clone()
    };
    let mut font_system = font_system().write().expect("Write font system");
    let (plain, plain_width) = spaced_clusters(font_system.raw(), &solid);
    let (clusters, width) = spaced_clusters(font_system.raw(), &spaced);
    drop(font_system);
    assert_eq!(clusters.len(), 9);
    assert!(close(width - plain_width, 9.0 * 1.5));
    for (i, ((range, x), (plain_range, plain_x))) in clusters.iter().zip(&plain).enumerate() {
        assert_eq!(range, plain_range);
        assert_eq!(range.len(), 1);
        assert!(close(x - plain_x, i as f32 * 1.5), "{i}: {x} vs {plain_x}");
    }
}

#[test]
fn the_fitting_prefix_stops_at_the_first_cluster_that_overflows() {
    let clusters = [(0, 1, 5.0), (1, 2, 5.0), (2, 5, 7.0), (5, 6, 5.0)];
    assert_eq!(fitting_prefix(&clusters, 0.0), 0);
    assert_eq!(fitting_prefix(&clusters, 9.9), 1);
    assert_eq!(fitting_prefix(&clusters, 10.0), 2);
    // A multi-byte cluster is kept or dropped whole.
    assert_eq!(fitting_prefix(&clusters, 16.0), 2);
    assert_eq!(fitting_prefix(&clusters, 17.0), 5);
    assert_eq!(fitting_prefix(&clusters, 100.0), 6);
}

#[test]
fn the_widget_reports_its_text_and_what_it_shows() {
    use iced::widget::container;
    use iced_test::selector::{Candidate, Target};

    super::testing::load_fonts();
    let title = "A practice with a title far too long for its card";
    let ui = container(studio_text(title).size(16).font(font::SEMIBOLD).ellipsis(1)).width(120);
    let mut ui = iced_test::simulator::<(), _, _>(ui);
    let full = ui.find(title).expect("the full text");
    assert!(full.bounds().width <= 120.0);
    let shown = ui
        .find(|candidate: Candidate<'_>| match candidate {
            Candidate::Text { content, .. } if content.ends_with(ELLIPSIS) => {
                Some((content.to_string(), Target::from(candidate)))
            }
            _ => None,
        })
        .expect("the cut text");
    assert!(title.starts_with(shown.0.trim_end_matches(ELLIPSIS)));
    assert_eq!(shown.1.bounds(), full.bounds());
}
