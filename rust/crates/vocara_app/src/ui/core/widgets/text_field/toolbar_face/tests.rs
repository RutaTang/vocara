//! The labels' face: the `trak` reading and the choice, on every platform;
//! on a Mac, the system UI face itself against what cosmic-text does.

use iced::advanced::graphics::text::cosmic_text::skrifa::raw::{FontData, FontRead};

use super::*;

/// A `trak` table with one horizontal set of `tracks`, each a track value
/// and its per-size values, over `sizes` (points).
fn trak_table(sizes: &[f32], tracks: &[(f32, &[i16])]) -> Vec<u8> {
    let fixed = |v: f32| ((v * 65536.0).round() as i32).to_be_bytes();
    let entries_at = 12 + 8;
    let sizes_at = entries_at + 8 * tracks.len();
    let values_at = sizes_at + 4 * sizes.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version 1.0
    out.extend_from_slice(&0u16.to_be_bytes()); // format
    out.extend_from_slice(&12u16.to_be_bytes()); // horizOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // vertOffset: none
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_be_bytes());
    out.extend_from_slice(&(sizes_at as u32).to_be_bytes());
    for (i, (track, _)) in tracks.iter().enumerate() {
        out.extend_from_slice(&fixed(*track));
        out.extend_from_slice(&(256 + i as u16).to_be_bytes()); // nameIndex
        let at = values_at + 2 * sizes.len() * i;
        out.extend_from_slice(&(at as u16).to_be_bytes());
    }
    for size in sizes {
        out.extend_from_slice(&fixed(*size));
    }
    for (_, values) in tracks {
        for value in *values {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
    out
}

fn track(table: &[u8], pt: f32) -> f32 {
    let trak = Trak::read(FontData::new(table)).expect("a trak table");
    normal_track(&trak, pt)
}

#[test]
fn the_normal_track_is_interpolated_between_its_sizes_as_harfbuzz_does() {
    // Apple-like: looser small, tighter large, 0 at 12 pt.
    let table = trak_table(
        &[9.0, 12.0, 17.0],
        &[
            (-1.0, &[0, -20, -60]),
            (0.0, &[10, 0, -30]),
            (1.0, &[40, 30, 0]),
        ],
    );
    assert_eq!(track(&table, 12.0), 0.0);
    assert_eq!(track(&table, 14.0), -12.0, "two fifths of the way to -30");
    assert_eq!(track(&table, 10.5), 5.0, "half way between 10 and 0");
    assert_eq!(track(&table, 6.0), 10.0, "below the first size: its value");
    assert_eq!(track(&table, 28.0), -30.0, "past the last size: its value");
}

#[test]
fn without_a_normal_track_the_two_around_it_are_interpolated() {
    let table = trak_table(&[12.0], &[(-1.0, &[-40]), (1.0, &[20])]);
    assert_eq!(track(&table, 12.0), -10.0);
    // One track only: it is the one.
    let table = trak_table(&[12.0], &[(0.5, &[8])]);
    assert_eq!(track(&table, 12.0), 8.0);
}

/// A system UI face with an `opsz` axis of `opsz` (min, default, max), if
/// any, and `tracking` added by cosmic-text's shaper.
fn face(opsz: Option<(f32, f32, f32)>, tracking: f32) -> SystemUiFace {
    let mut axes = vec![Axis {
        tag: "wght".to_string(),
        min: 1.0,
        default: 400.0,
        max: 1000.0,
    }];
    if let Some((min, default, max)) = opsz {
        axes.push(Axis {
            tag: "opsz".to_string(),
            min,
            default,
            max,
        });
    }
    SystemUiFace {
        id: ID::dummy(),
        family: ".SF NS".to_string(),
        axes,
        units_per_em: 2048,
        line_height: 1.19,
        default_tracking: tracking,
        label_tracking: -22.0,
    }
}

#[test]
fn the_system_font_draws_as_flutter_only_at_its_default_optical_size_untracked() {
    // The axis starts above 14 pt at its default: 12 and 14 pt both clamp
    // to it, which is what cosmic-text draws.
    assert!(face(Some((17.0, 17.0, 28.0)), 0.0).draws_as_flutter(14.0));
    // Its default is the large design: cosmic-text would draw that.
    assert!(!face(Some((17.0, 28.0, 96.0)), 0.0).draws_as_flutter(14.0));
    // 14 pt is its default, but Skia's 12 pt is not.
    assert!(!face(Some((6.0, 14.0, 96.0)), 0.0).draws_as_flutter(14.0));
    // No optical size to set.
    assert!(face(None, 0.0).draws_as_flutter(14.0));
    // A face cosmic-text's shaper would track (where HarfBuzz before 2025
    // did not).
    assert!(!face(Some((17.0, 17.0, 28.0)), 12.0).draws_as_flutter(14.0));
    assert_eq!(
        face(Some((17.0, 17.0, 28.0)), 0.0).core_text_opsz(14.0),
        Some(17.0)
    );
    assert_eq!(face(None, 0.0).core_text_opsz(14.0), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_labels_are_in_inter() {
    assert_eq!(label_face(), &inter());
    assert_eq!(label_face().font, font::REGULAR);
    assert_eq!(
        super::super::toolbar::item_height(),
        2.0 + LABEL_SIZE * LABEL_LINE_HEIGHT + 5.0
    );
}

/// On a Mac (CI's runner): reads the system UI face, prints it, and checks
/// each step of the choice against what cosmic-text does with the face —
/// which face and advance it lays a glyph out in, the tracking its shaper
/// adds — and that the labels got the face exactly when it draws as
/// Flutter's did.
#[cfg(target_os = "macos")]
#[test]
fn the_labels_take_the_system_font_exactly_when_cosmic_text_draws_it_as_flutter_did() {
    use iced::advanced::graphics::text::cosmic_text::skrifa::instance::Size as FontSize;
    use iced::advanced::graphics::text::font_system;
    use iced::advanced::text::{Alignment, Wrapping};
    use iced::font::{Family, Stretch, Style, Weight};
    use iced::Size;

    use crate::ui::core::widgets::studio_text::{lay_out, Spec};

    // Chosen first: it takes the font system's lock.
    let chosen = label_face().clone();
    let mut fonts = font_system().write().expect("the font system");
    let face = SystemUiFace::read(fonts.raw().db())
        .expect("SFNS.ttf, the system UI font, loaded from /System/Library/Fonts");
    println!("system UI face: {face:#?}");
    let named: Vec<String> = fonts
        .raw()
        .db()
        .with_face_data(face.id, |data, index| {
            let font = FontRef::from_index(data, index).expect("the face parses");
            font.named_instances()
                .iter()
                .map(|instance| {
                    let name = font
                        .localized_strings(instance.subfamily_name_id())
                        .english_or_first()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    format!("{name}: {:?}", instance.user_coords().collect::<Vec<_>>())
                })
                .collect()
        })
        .unwrap_or_default();
    println!("named instances: {named:#?}");

    // The design: where CoreText puts 14 pt and Skia's 12 pt, against the
    // default cosmic-text draws.
    let design = face.opsz().is_none_or(|axis| {
        face.core_text_opsz(LABEL_SIZE) == Some(axis.default)
            && face.core_text_opsz(DEFAULT_PT) == Some(axis.default)
    });
    println!(
        "opsz: {:?}; CoreText at {LABEL_SIZE} pt: {:?}, at {DEFAULT_PT} pt: {:?}; \
         the default {} where both are",
        face.opsz(),
        face.core_text_opsz(LABEL_SIZE),
        face.core_text_opsz(DEFAULT_PT),
        if design { "is" } else { "is NOT" }
    );
    println!(
        "trak: normal track {} units at {DEFAULT_PT} pt (what cosmic-text adds), {} at \
         {LABEL_SIZE} pt (CoreText's); the toolbar's letter spacing is {} units",
        face.default_tracking,
        face.label_tracking,
        super::super::toolbar::LABEL_SPACING / LABEL_SIZE * f32::from(face.units_per_em)
    );
    println!(
        "line box: {} em (Inter's {LABEL_LINE_HEIGHT})",
        face.line_height
    );

    // What cosmic-text does: lays each glyph out in the face, with the
    // advance of the default optical size (weight 400), plus the track
    // its shaper adds at 12 pt; and whether that advance is CoreText's.
    let family: &'static str = Box::leak(face.family.clone().into_boxed_str());
    let system = Font {
        family: Family::Name(family),
        weight: Weight::Normal,
        stretch: Stretch::Normal,
        style: Style::Normal,
    };
    let upem = f32::from(face.units_per_em);
    let mut differs = Vec::new();
    for ch in ['H', 'o', 'n', 'e', 'C', 'y'] {
        let text = ch.to_string();
        let spec = Spec {
            content: &text,
            size: LABEL_SIZE,
            line_height: LABEL_SIZE * 1.5,
            font: system,
            align_x: Alignment::Default,
            wrapping: Wrapping::None,
            letter_spacing: 0.0,
            tabular: false,
            max_lines: None,
        };
        let laid = lay_out(fonts.raw(), &spec, Size::INFINITE);
        let glyph = laid
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().cloned())
            .next()
            .unwrap_or_else(|| panic!("{ch} lays out a glyph"));
        assert_eq!(
            glyph.font_id, face.id,
            "cosmic-text sets {ch} in the system UI face it was asked for"
        );
        let (cosmic, core_text) = fonts
            .raw()
            .db()
            .with_face_data(face.id, |data, index| {
                let font = FontRef::from_index(data, index).expect("the face parses");
                let gid = font.charmap().map(ch).expect("the face has the letter");
                let size = FontSize::new(LABEL_SIZE);
                let mut settings = vec![(Tag::new(b"wght"), 400.0)];
                let cosmic = font.axes().location(settings.clone());
                if let Some(opsz) = face.core_text_opsz(LABEL_SIZE) {
                    settings.push((Tag::new(b"opsz"), opsz));
                }
                let core_text = font.axes().location(settings);
                let advance = |location| {
                    font.glyph_metrics(size, &location)
                        .advance_width(gid)
                        .expect("an advance")
                };
                (advance(cosmic), advance(core_text))
            })
            .expect("the face's data");
        let tracked = cosmic + face.default_tracking * LABEL_SIZE / upem;
        assert!(
            (glyph.w - tracked).abs() < 0.01,
            "cosmic-text lays {ch} out at the default optical size's advance plus its shaper's \
             12 pt track: {} px, against {cosmic} + {} = {tracked}",
            glyph.w,
            face.default_tracking * LABEL_SIZE / upem
        );
        if (cosmic - core_text).abs() >= 0.01 {
            differs.push((ch, cosmic, core_text));
        }
    }
    assert!(
        !design || differs.is_empty(),
        "at the default optical size every advance is CoreText's at {LABEL_SIZE} pt: {differs:?}"
    );
    println!("advances cosmic-text / CoreText where they differ: {differs:?}");
    drop(fonts);

    // The choice.
    let expected = design && face.default_tracking == 0.0;
    assert_eq!(face.draws_as_flutter(LABEL_SIZE), expected);
    if expected {
        assert_eq!(
            chosen,
            LabelFace {
                font: system,
                line_height: face.line_height,
                system: true,
            },
            "the labels are in the system UI font, at its line box"
        );
    } else {
        assert_eq!(chosen, inter(), "the labels stay in Inter");
    }
    assert_eq!(
        super::super::toolbar::item_height(),
        2.0 + LABEL_SIZE * chosen.line_height + 5.0,
        "an item's height is its label's line box and padding"
    );
    println!(
        "verdict: the toolbar's labels are in {}",
        if chosen.system {
            "the system UI font: cosmic-text draws it as Flutter did"
        } else {
            "Inter: cosmic-text would not draw the system UI font as Flutter did"
        }
    );
}
