//! The labels' face: its line at the labels' size, worked out on the
//! variable font made up for the system font's tests
//! (`system_font::made_up`); off a Mac, Inter on SkParagraph's line; on a
//! Mac, the system UI face against what CoreText gave Skia
//! (`system_font::tests::check_against_core_text`).

use super::*;
use crate::ui::core::system_font::made_up::{font_system_of, variable_font};

#[test]
fn the_labels_line_is_their_faces_at_14_px() {
    let (mut fonts, id) = font_system_of(variable_font((6.0, 28.0, 96.0)));
    let face = system_font::register(&mut fonts, id, Weight::Normal).expect("registered");
    let label = LabelFace::of(&face);
    assert!(label.system);
    assert_eq!(label.font, face.font);
    // At 14 px: (872.7 + 200 + 40) × 0.014 = 15.58, a 16 px line, its
    // glyphs on the whole pixel nearest 0.28 + 12.22 below its top.
    assert_eq!((label.line_height, label.baseline), (16.0, 12.0));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_labels_are_in_inter_on_skparagraphs_line() {
    use crate::ui::core::theme::font;

    assert_eq!(label_face(), &inter());
    assert_eq!(label_face().font, font::REGULAR);
    assert!(!label_face().system);
    let inter = system_font::Instance::of(
        &iced::advanced::graphics::text::cosmic_text::skrifa::FontRef::new(font::FACES[0])
            .expect("Inter parses"),
        Weight::Normal,
    )
    .expect("Inter's");
    // Inter's hhea: 2728 up, 680 down, no gap, on 2816 units: 16.94 px at
    // 14, a 17 px line.
    assert_eq!(
        (
            inter.units_per_em,
            inter.ascent,
            inter.descent,
            inter.leading
        ),
        (2816, 2728.0, 680.0, 0.0)
    );
    assert_eq!(label_face().line_height, 17.0);
    // Its glyphs on the whole pixel nearest 13.56 below its top.
    assert_eq!(label_face().baseline, 14.0);
    assert_eq!(super::super::toolbar::item_height(), 2.0 + 17.0 + 5.0);
}

/// On a Mac (CI's runner): asks CoreText for the fonts Skia made for the
/// labels (its typeface of `.AppleSystemUIFont` at w400, its exact copy at
/// 14 pt) and checks the labels against them (the face, the instance, each
/// glyph's advance, the positioning, the line), and the items' height
/// against that line.
#[cfg(target_os = "macos")]
#[test]
fn the_labels_are_drawn_at_the_instance_and_metrics_coretext_gave_skia() {
    use super::super::toolbar::{item_height, Item};
    use crate::ui::core::system_font::tests::{check_against_core_text, LabelStyle};

    const SAMPLES: [&str; 3] = ["Cut", "Copy", "Paste"];
    assert_eq!(
        SAMPLES,
        [Item::Cut, Item::Copy, Item::Paste].map(Item::label)
    );
    let face = check_against_core_text(&LabelStyle {
        name: "the text toolbar's labels (`_kToolbarButtonFontStyle`: 14)",
        size: LABEL_SIZE,
        weight: Weight::Normal,
        tabular: false,
        samples: &SAMPLES,
    });
    let chosen = label_face();
    assert!(
        chosen.system,
        "on a Mac the labels are in the system UI font"
    );
    assert_eq!(chosen, &LabelFace::of(&face));
    let (height, _) = face.line(LABEL_SIZE);
    assert_eq!(item_height(), 2.0 + height + 5.0);
}
