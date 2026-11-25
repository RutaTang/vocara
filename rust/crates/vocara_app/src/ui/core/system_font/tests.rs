//! The system UI font's faces: on every platform, Skia's weights of
//! CoreText's traits, Flutter's instance of each weight, its line and
//! untracked shaping, worked out on a variable font made up for the tests
//! (`made_up`) registered in a font system of its own, and the line gap
//! every line takes; on a Mac, every label's face against what CoreText
//! gave Skia.

use iced::advanced::graphics::text::cosmic_text::skrifa::instance::Size as FontSize;
use iced::advanced::graphics::text::cosmic_text::SwashCache;
use iced::advanced::text::{Alignment, Wrapping};
use iced::Size;

use super::made_up::{
    advance, advances, font_system_of, made_up, variable_font, MadeUp, FAMILY, TRACK_12, UPEM,
};
use super::*;
use crate::ui::core::widgets::studio_text::{lay_out, Spec};

/// Every weight, `FontWeight.w100` to `w900`.
const WEIGHTS: [Weight; 9] = [
    Weight::Thin,
    Weight::ExtraLight,
    Weight::Light,
    Weight::Normal,
    Weight::Medium,
    Weight::Semibold,
    Weight::Bold,
    Weight::ExtraBold,
    Weight::Black,
];

/// `SFNS.ttf`'s named weights at a normal width and grade, in its order:
/// Ultralight, Thin, Light, Regular, Medium, Semibold, Bold, Heavy, Black.
const SF_WGHTS: [f32; 9] = [
    30.925, 110.725, 274.315, 400.0, 510.0, 590.0, 700.0, 860.0, 1000.0,
];

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn skia_maps_coretexts_weight_traits_onto_css_weights_through_appkits_constants() {
    // Each constant is its hundred.
    for (i, ct) in NS_FONT_WEIGHTS.iter().enumerate() {
        assert_eq!(css_weight(*ct), 100 * i as i32, "{ct}");
    }
    // Linearly between them, rounded half up.
    assert_eq!(css_weight(0.35), 650);
    assert_eq!(css_weight(0.23 + 0.07 * 0.004), 500);
    assert_eq!(css_weight(0.23 + 0.07 * 0.006), 501);
    // Beyond either end.
    assert_eq!(css_weight(-1.5), 0);
    assert_eq!(css_weight(1.5), 1000);
}

#[test]
fn coretext_weighs_san_franciscos_named_weights_with_appkits_constants() {
    // Light up, each named weight is the CSS weight of its name: Skia
    // matches FontWeight.w600 to Semibold (wght 590), not to wght 600.
    let expected = [101, 166, 300, 400, 500, 600, 700, 800, 900];
    for (wght, css) in SF_WGHTS.iter().zip(expected) {
        assert_eq!(css_weight(ct_weight(*wght)), css, "wght {wght}");
    }
    // At WebKit's measurements, AppKit's constants.
    for (ct, wght) in SAN_FRANCISCO_WEIGHTS {
        assert!((ct_weight(wght as f32) - ct).abs() < 1e-9, "wght {wght}");
    }
    // Beyond them, the nearest.
    assert_eq!(ct_weight(1.0), -0.8);
    assert_eq!(ct_weight(1000.0), 0.62);
}

/// A made-up face named as `SFNS.ttf` is: its weights at a normal width
/// (`wdth` 100), its optical size axis 17 to 96 at 28; and, before them in
/// its order, condensed ones (`wdth` 75) at the weights a label asks for.
fn san_francisco_like() -> Vec<u8> {
    let mut instances: Vec<Vec<f32>> = [400.0, 600.0, 700.0]
        .into_iter()
        .map(|wght| vec![28.0, wght, 75.0])
        .collect();
    instances.extend(SF_WGHTS.iter().map(|wght| vec![28.0, *wght, 100.0]));
    MadeUp {
        opsz: (17.0, 28.0, 96.0),
        width: true,
        instances,
    }
    .build()
}

#[test]
fn each_weight_is_the_named_instance_skia_matched_at_its_optical_size() {
    let data = san_francisco_like();
    let face = FontRef::new(&data).expect("the made-up font parses");
    for (weight, wght) in WEIGHTS.iter().zip(SF_WGHTS) {
        let instance = Instance::of(&face, *weight).expect("an instance");
        // The named weight nearest it at a normal width: Thin's 166 is
        // FontWeight.w200's nearest, Ultralight's 101 w100's.
        assert!(
            instance.coord(b"wght").is_some_and(|v| close(v, wght)),
            "{weight:?}: {instance:?}"
        );
        assert_eq!(instance.coord(b"wdth"), Some(100.0), "{weight:?}");
        // The 12 pt design, clamped to the axis: its 17 pt one.
        assert_eq!(instance.coord(b"opsz"), Some(17.0), "{weight:?}");
        assert_eq!(
            instance.skia_weight,
            Some(css_weight(ct_weight(wght))),
            "{weight:?}"
        );
    }
}

#[test]
fn a_face_without_named_instances_is_its_default_instance_at_the_12_pt_optical_size() {
    let instance = |opsz, weight| {
        let data = variable_font(opsz);
        Instance::of(
            &FontRef::new(&data).expect("the made-up font parses"),
            weight,
        )
        .expect("an instance")
    };
    // The axis reaches down to 12: Skia's 12 pt optical size, not the
    // size CoreText would pick by itself, nor the default.
    let low = instance((6.0, 28.0, 96.0), Weight::Normal);
    assert_eq!(low.coord(b"opsz"), Some(12.0));
    assert_eq!(low.coord(b"wght"), Some(400.0));
    assert_eq!(low.coord(b"wdth"), None);
    assert_eq!(low.skia_weight, None);
    // CoreText's one descriptor of it, whatever the weight asked for.
    let bold = instance((6.0, 28.0, 96.0), Weight::Bold);
    assert_eq!(bold.coord(b"wght"), Some(400.0));
    // It starts above 12: its lowest.
    assert_eq!(
        instance((17.0, 28.0, 96.0), Weight::Normal).coord(b"opsz"),
        Some(17.0)
    );
    // A face without the axes keeps none.
    let inter = FontRef::new(font::FACES[0]).expect("Inter parses");
    assert!(Instance::of(&inter, Weight::Normal)
        .expect("Inter's")
        .coords
        .is_empty());
}

#[test]
fn the_line_is_skparagraphs_from_the_hhea_metrics_and_their_mvar_deltas_there() {
    let data = variable_font((6.0, 28.0, 96.0));
    let instance =
        Instance::of(&FontRef::new(&data).expect("parses"), Weight::Normal).expect("an instance");
    // 12 on an axis from 6 to 28: 16/22 of the way down to where the
    // ascender is 100 higher.
    let along = 16.0 / 22.0;
    assert!(
        (instance.ascent - (800.0 + 100.0 * along)).abs() < 0.01,
        "{instance:?}"
    );
    assert_eq!((instance.descent, instance.leading), (200.0, 40.0));
    // At 14 px: (872.7 + 200 + 40) × 0.014 = 15.58, a 16 px line, its
    // glyphs on the whole pixel nearest 0.28 + 12.22 below its top.
    let (height, baseline) = instance.line(14.0);
    assert_eq!(height, 16.0);
    assert_eq!(baseline, 12.0);
}

#[test]
fn the_registered_face_shapes_the_labels_at_the_instance_untracked() {
    let (mut fonts, id) = font_system_of(variable_font((6.0, 28.0, 96.0)));
    let face = register(&mut fonts, id, Weight::Normal).expect("registered");
    assert!(face.system);
    assert_eq!(face.font.family, Family::Name(family(Weight::Normal)));
    assert_eq!(
        fonts.face_instance(id),
        None,
        "the face itself is untouched"
    );
    let data = variable_font((6.0, 28.0, 96.0));
    let font = FontRef::new(&data).expect("parses");
    let location = font.axes().location(face.instance.coords.iter().copied());
    let size = 14.0;
    let metrics = font.glyph_metrics(FontSize::new(size), &location);
    let scale = size / f32::from(UPEM);
    for label in ["Cut", "Copy", "Paste"] {
        let drawn = advances(&mut fonts, face.font, size, 20.0, false, label);
        let upstream = advances(&mut fonts, made_up(), size, 20.0, false, label);
        assert_eq!(drawn.len(), label.len(), "{label}: {drawn:?}");
        for ((gid, w), (upstream_gid, upstream_w)) in drawn.iter().zip(&upstream) {
            assert_eq!(gid, upstream_gid);
            // The instance's advance (HVAR at 12 pt: 58.2 units wider),
            // to within harfrust's rounding of the delta to a unit, and
            // no track.
            let expected = metrics.advance_width((*gid).into()).expect("an advance");
            assert!(
                (w - expected).abs() < 0.01,
                "{label} glyph {gid}: {w}, not {expected}"
            );
            assert!((expected - (f32::from(advance(*gid)) + 58.18) * scale).abs() < 0.01);
            // Upstream: the default's advance, plus the 12 pt track.
            let tracked = f32::from(advance(*gid) + TRACK_12 as u16) * scale;
            assert!(
                (upstream_w - tracked).abs() < 0.01,
                "{upstream_w} {tracked}"
            );
        }
    }
}

#[test]
fn each_weight_is_registered_in_a_family_of_its_own_at_its_instance() {
    let (mut fonts, id) = font_system_of(san_francisco_like());
    for (weight, wght) in [
        (Weight::Normal, 400.0),
        (Weight::Semibold, 590.0),
        (Weight::Bold, 700.0),
    ] {
        let face = register(&mut fonts, id, weight).expect("registered");
        assert_eq!(face.font.family, Family::Name(family(weight)));
        let copy = fonts
            .db()
            .faces()
            .find(|info| info.families.iter().any(|(name, _)| name == family(weight)))
            .map(|info| info.id)
            .expect("the copy");
        let set = fonts.face_instance(copy).expect("an instance").clone();
        assert!(set.untracked, "{weight:?}");
        let at = |tag: &[u8; 4]| set.coords.iter().find(|(t, _)| t == tag).map(|(_, v)| *v);
        assert!(
            at(b"wght").is_some_and(|v| close(v, wght)),
            "{weight:?}: {set:?}"
        );
        assert_eq!(at(b"opsz"), Some(17.0), "{weight:?}");
        assert_eq!(at(b"wdth"), Some(100.0), "{weight:?}");
    }
    assert_eq!(
        fonts.face_instance(id),
        None,
        "the face itself is untouched"
    );
    // Three faces of three families besides the face.
    assert_eq!(fonts.db().faces().count(), 4);
    assert!(fonts
        .db()
        .faces()
        .any(|info| info.families.iter().any(|(name, _)| name == FAMILY)));
}

#[test]
fn the_registered_face_is_drawn_at_the_instance() {
    let (mut fonts, id) = font_system_of(variable_font((6.0, 28.0, 96.0)));
    let face = register(&mut fonts, id, Weight::Normal).expect("registered");
    // At 100 px the box of `C` (glyph 1) is 0.1 × (410 + 58.2) px wide at
    // the instance, against 41 at the default.
    let width = |fonts: &mut FontSystem, font: Font| {
        let spec = Spec {
            content: "C",
            size: 100.0,
            line_height: 120.0,
            font,
            align_x: Alignment::Default,
            wrapping: Wrapping::None,
            letter_spacing: 0.0,
            tabular: false,
            max_lines: None,
        };
        let laid = lay_out(fonts, &spec, Size::INFINITE);
        let glyph = laid
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().cloned())
            .next()
            .expect("a glyph");
        let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
        let image = SwashCache::new()
            .get_image_uncached(fonts, key)
            .expect("an image");
        image.placement.width as f32
    };
    let instance = width(&mut fonts, face.font);
    let default = width(&mut fonts, made_up());
    assert!((default - 41.0).abs() <= 1.0, "{default}");
    assert!((instance - 46.8).abs() <= 1.0, "{instance}");
}

/// `text`'s one line in `font` at `size` px, `line_height` tall (zero: no
/// height): its height and its baseline below its top.
fn line_of(
    fonts: &mut FontSystem,
    font: Font,
    size: f32,
    line_height: f32,
    text: &str,
) -> (f32, f32) {
    let spec = Spec {
        content: text,
        size,
        line_height,
        font,
        align_x: Alignment::Default,
        wrapping: Wrapping::None,
        letter_spacing: 0.0,
        tabular: false,
        max_lines: None,
    };
    let laid = lay_out(fonts, &spec, Size::INFINITE);
    let run = laid.buffer.layout_runs().next().expect("a line");
    (run.line_height, run.line_y - run.line_top)
}

#[test]
fn a_line_takes_its_fonts_line_gap_half_above_and_half_below() {
    // The made-up face's default instance at 30 px: ascent 24, descent 6,
    // line gap 1.2.
    let (mut fonts, _) = font_system_of(variable_font((6.0, 28.0, 96.0)));
    // A TextPainter's line: 31.2, rounded, tall, its glyphs 0.6 + 24
    // below its top, rounded (without the gap: 30, and 24).
    assert_eq!(
        line_of(&mut fonts, made_up(), 30.0, 0.0, "Cut"),
        (31.0, 25.0)
    );
    // With a height of 1: the 31.2 scaled to 30, the 24.6 above to 23.65.
    assert_eq!(
        line_of(&mut fonts, made_up(), 30.0, 30.0, "Cut"),
        (30.0, 24.0)
    );
    // An empty paragraph's (`computeEmptyMetrics`): its leading the gap.
    assert_eq!(line_of(&mut fonts, made_up(), 30.0, 0.0, ""), (31.0, 25.0));
    // With a height, the three scaled by 30 over their sum rounded, 31.
    assert_eq!(line_of(&mut fonts, made_up(), 30.0, 30.0, ""), (30.0, 24.0));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_every_label_is_in_inter_at_its_weight() {
    for weight in WEIGHTS {
        let face = face(weight);
        assert_eq!(face, &inter(weight));
        assert!(!face.system);
        assert_eq!(face.font, font::inter(weight), "{weight:?}");
        assert_eq!(font(weight), font::inter(weight));
        // Inter's hhea: 2728 up, 680 down, no gap, on 2816 units.
        let instance = &face.instance;
        assert_eq!(
            (
                instance.units_per_em,
                instance.ascent,
                instance.descent,
                instance.leading
            ),
            (2816, 2728.0, 680.0, 0.0)
        );
    }
    // The weights the labels use are the faces they were drawn in before.
    assert_eq!(font(Weight::Normal), font::REGULAR);
    assert_eq!(font(Weight::Semibold), font::SEMIBOLD);
    assert_eq!(font(Weight::Bold), font::BOLD);
}

// ------------------------------------------------------------ on a Mac

/// A label Flutter drew with a `TextPainter` in the system UI font: what it
/// is, its size, weight and figures, and texts it shows.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct LabelStyle {
    pub name: &'static str,
    pub size: f32,
    pub weight: Weight,
    pub tabular: bool,
    pub samples: &'static [&'static str],
}

/// Every canvas label's style (`git grep TextPainter` in the Dart app's
/// `client/lib`: each one's `TextStyle`).
#[cfg(target_os = "macos")]
pub(crate) const CANVAS_LABELS: [LabelStyle; 8] = [
    LabelStyle {
        name: "the EQ's decade labels (fx_panel.dart `_decadeLabels`: 8.5)",
        size: 8.5,
        weight: Weight::Normal,
        tabular: false,
        samples: &["100", "1k", "10k"],
    },
    LabelStyle {
        name: "the pitch lane's octaves and cents (pitch_lane.dart `_label`: 9)",
        size: 9.0,
        weight: Weight::Normal,
        tabular: false,
        samples: &["C2", "C4", "+12¢", "−7¢"],
    },
    LabelStyle {
        name: "the vowel chart's references (spectrogram_painter.dart \
               `VowelSpacePainter`: 9)",
        size: 9.0,
        weight: Weight::Normal,
        tabular: false,
        samples: &["i", "e", "a", "o", "u"],
    },
    LabelStyle {
        name: "the formant markers (spectrogram_painter.dart \
               `PowerSpectrumPainter`: 9, w700)",
        size: 9.0,
        weight: Weight::Bold,
        tabular: false,
        samples: &["F1", "F2", "F3"],
    },
    LabelStyle {
        name: "the voiceprint's frequency axis (voiceprint_view.dart \
               `_FreqAxisPainter`: 10)",
        size: 10.0,
        weight: Weight::Normal,
        tabular: false,
        samples: &["1k", "4k", "8k"],
    },
    LabelStyle {
        name: "the time ruler (waveform_painter.dart `TimeRulerPainter`: 10, \
               tabular figures)",
        size: 10.0,
        weight: Weight::Normal,
        tabular: true,
        samples: &["00:05", "01:30", "12:48"],
    },
    LabelStyle {
        name: "the passaggio (voice_profile_view.dart `_profileText`: 10, w600)",
        size: 10.0,
        weight: Weight::Semibold,
        tabular: false,
        samples: &["E4", "F♯4"],
    },
    LabelStyle {
        name: "the range's ends (voice_profile_view.dart `_profileText`: 11, \
               w600)",
        size: 11.0,
        weight: Weight::Semibold,
        tabular: false,
        samples: &["A2", "C♯5", "G♯5"],
    },
];

/// On a Mac: asks CoreText for the fonts Skia made for labels of `style`
/// (its typeface of `.AppleSystemUIFont` at the style's weight, the exact
/// copy at its size) and checks the face the port sets them in against
/// them: the descriptor matched (Skia's style of it the instance's), no
/// fake bold, the file, the instance (every axis at the exact copy's
/// variation), each glyph's advance and font, the positioning HarfBuzz
/// applied at the typeface's own variation, and the line (CoreText's
/// ascent, descent and leading, made SkParagraph's line, against the line
/// cosmic-text lays the face out on and [`Instance::line`]). Prints what
/// it compares. The face, for the caller's own checks.
#[cfg(target_os = "macos")]
pub(crate) fn check_against_core_text(style: &LabelStyle) -> Face {
    use iced::advanced::graphics::text::cosmic_text::fontdb::FaceInfo;
    use iced::advanced::graphics::text::font_system;

    use super::core_text::{self, tag_name};
    use crate::ui::core::text;

    let wanted = i32::from(weight_value(style.weight));
    println!(
        "\n== {}: {} px, w{wanted}{}",
        style.name,
        style.size,
        if style.tabular { ", tnum" } else { "" }
    );
    // Chosen first: it takes the font system's lock.
    let chosen = face(style.weight).clone();
    println!("its face: {:?}", chosen.font);
    println!("its instance: {:?}", chosen.instance);
    assert!(
        chosen.system,
        "{}: on a Mac the labels are in the system UI font",
        style.name
    );

    // AppKit's weights, which Skia maps CoreText's traits through.
    let appkit = core_text::ns_font_weights();
    for (ours, theirs) in NS_FONT_WEIGHTS.iter().zip(appkit) {
        assert!(
            (ours - theirs).abs() < 1e-6,
            "AppKit's NSFontWeight constants {appkit:?}, not {NS_FONT_WEIGHTS:?}"
        );
    }

    // Skia's typeface: the family's descriptor nearest the weight.
    let family = core_text::system_font_family(14.0);
    println!("[NSFont systemFontOfSize:14].familyName: {family}");
    let descriptors = core_text::family_descriptors(&family);
    println!("CoreText's descriptors of {family}, in its order:");
    for d in &descriptors {
        println!("  {d:?}");
    }
    let matched = core_text::skia_match(&descriptors, wanted);
    println!("Skia's match for w{wanted}: {matched:?}");
    assert_eq!(
        chosen.instance.skia_weight,
        Some(matched.style.0),
        "{}: the named instance's weight is Skia's of the descriptor it matched ({matched:?})",
        style.name
    );
    let typeface = matched.skia_typeface();
    println!(
        "Skia's typeface: {} at {} pt, Skia's weight {}, variation {:?}",
        typeface.postscript_name(),
        typeface.size(),
        typeface.skia_weight(),
        typeface.variation()
    );
    assert!(
        !(wanted >= 600 && wanted - typeface.skia_weight() >= 200),
        "{}: SkParagraph would fake the bold of a w{} typeface",
        style.name,
        typeface.skia_weight()
    );
    let (copy, opsz) = typeface.skia_exact_copy(f64::from(style.size));
    let path = copy.path().unwrap_or_default();
    println!(
        "Skia's exact copy at {} pt: optical size {opsz}, {path}, variation {:?}",
        style.size,
        copy.variation()
    );
    assert!(
        path.ends_with("SFNS.ttf"),
        "{}: CoreText draws {family} from {path}",
        style.name
    );

    let (ascent, descent, leading, sk_line) = {
        let mut fonts = font_system().write().expect("the font system");
        let fonts = fonts.raw();
        let id = system_ui_face(fonts.db()).expect("SFNS.ttf loaded from /System/Library/Fonts");
        let (data, index) = fonts
            .db()
            .with_face_data(id, |data, index| (data.to_vec(), index))
            .expect("the face's data");
        let face = FontRef::from_index(&data, index).expect("SFNS.ttf parses");
        let instance = &chosen.instance;
        let axes: Vec<(Tag, f32, f32, f32)> = face
            .axes()
            .iter()
            .map(|a| (a.tag(), a.min_value(), a.default_value(), a.max_value()))
            .collect();
        println!("axes (min, default, max): {axes:?}");

        // Every axis where CoreText set the exact copy's (an axis it does
        // not report is at its default).
        let at = |variation: &[(u32, f64)], tag: Tag, default: f32| {
            variation
                .iter()
                .find(|(t, _)| *t == u32::from_be_bytes(tag.to_be_bytes()))
                .map_or(default, |(_, v)| *v as f32)
        };
        let copy_variation = copy.variation();
        for (tag, _, default, _) in &axes {
            let ours = instance.coord(&tag.to_be_bytes()).expect("each axis");
            let core_text = at(&copy_variation, *tag, *default);
            assert!(
                (ours - core_text).abs() < 0.01,
                "{}: {tag}: the labels' {ours}, CoreText's {core_text}",
                style.name
            );
        }
        // HarfBuzz positioned the glyphs at the typeface's own variation.
        let typeface_variation = typeface.variation();
        let shaped_at: Vec<(Tag, f32)> = axes
            .iter()
            .map(|(tag, _, default, _)| (*tag, at(&typeface_variation, *tag, *default)))
            .collect();
        println!(
            "HarfBuzz's variation: {:?}",
            shaped_at
                .iter()
                .map(|(t, v)| format!("{} {v}", tag_name(u32::from_be_bytes(t.to_be_bytes()))))
                .collect::<Vec<_>>()
        );
        let same = shaped_at.iter().all(|(tag, v)| {
            instance
                .coord(&tag.to_be_bytes())
                .is_some_and(|ours| (ours - v).abs() < 0.01)
        });
        // A copy of the face shaped there, to compare its positioning with.
        let harfbuzz = if same {
            None
        } else {
            let name = ".Vocara HarfBuzz Variation";
            let existing = fonts
                .db()
                .faces()
                .find(|info| info.families.iter().any(|(n, _)| n == name))
                .map(|info| info.id);
            let copy_id = existing.unwrap_or_else(|| {
                let info = fonts.db().face(id).expect("the face").clone();
                fonts.db_mut().push_face_info(FaceInfo {
                    families: vec![(name.into(), Language::English_UnitedStates)],
                    ..info
                })
            });
            fonts.set_face_instance(
                copy_id,
                Some(FaceInstance {
                    coords: shaped_at
                        .iter()
                        .map(|(t, v)| (t.to_be_bytes(), *v))
                        .collect(),
                    untracked: true,
                    features: Vec::new(),
                }),
            );
            Some(Font {
                family: Family::Name(name),
                ..Font::DEFAULT
            })
        };
        // The registered copy the labels are drawn with.
        let ours_id = fonts
            .db()
            .faces()
            .find(|info| {
                info.families
                    .iter()
                    .any(|(n, _)| n == family_of(&chosen.font))
            })
            .map(|info| info.id)
            .expect("the labels' face");

        let size = FontSize::new(style.size);
        let ours_at = face.axes().location(instance.coords.iter().copied());
        let harfbuzz_at = face.axes().location(shaped_at.iter().copied());
        let nominal = |location, gid: u16| {
            face.glyph_metrics(size, location)
                .advance_width(gid.into())
                .expect("an advance")
        };
        for sample in style.samples {
            let spec = Spec {
                content: sample,
                size: style.size,
                line_height: 0.0,
                font: chosen.font,
                align_x: Alignment::Default,
                wrapping: Wrapping::None,
                letter_spacing: 0.0,
                tabular: style.tabular,
                max_lines: None,
            };
            let laid = lay_out(fonts, &spec, Size::INFINITE);
            let drawn: Vec<(u16, f32, _)> = laid
                .buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter().map(|g| (g.glyph_id, g.w, g.font_id)))
                .collect();
            assert_eq!(
                drawn.len(),
                sample.chars().count(),
                "{}: {sample}: {drawn:?}",
                style.name
            );
            let positioned =
                harfbuzz.map(|font| advances(fonts, font, style.size, 0.0, style.tabular, sample));
            for (i, (gid, w, font_id)) in drawn.iter().enumerate() {
                assert_eq!(
                    *font_id, ours_id,
                    "{}: {sample} glyph {i} is the face's, not a fallback's",
                    style.name
                );
                // The instance's advance is CoreText's.
                let ours = nominal(&ours_at, *gid);
                let core_text = copy.advance(*gid) as f32;
                assert!(
                    (ours - core_text).abs() < 0.01,
                    "{}: {sample} glyph {gid}: advance {ours}, CoreText's {core_text}",
                    style.name
                );
                // What the shaper added to it, HarfBuzz's at its variation.
                let added = w - ours;
                if let Some(positioned) = &positioned {
                    let (hb_gid, hb_w) = positioned[i];
                    assert_eq!(hb_gid, *gid, "{}: {sample}: the same glyphs", style.name);
                    let hb_added = hb_w - nominal(&harfbuzz_at, hb_gid);
                    assert!(
                        (added - hb_added).abs() < 0.01,
                        "{}: {sample} glyph {gid}: positioned {added} past its advance, \
                         HarfBuzz {hb_added}",
                        style.name
                    );
                }
                println!("{sample} glyph {gid}: {w} px ({ours} + {added})");
            }
        }

        // The line: CoreText's metrics of the exact copy, as Skia keeps
        // them (`float`s), made SkParagraph's line without a height.
        let (ascent, descent, leading) = copy.metrics();
        let scale = style.size / f32::from(instance.units_per_em);
        println!(
            "CoreText's ascent, descent, leading at {} pt: {ascent} {descent} {leading}; \
             the instance's {} {} {}",
            style.size,
            instance.ascent * scale,
            instance.descent * scale,
            instance.leading * scale
        );
        for (what, ours, core_text) in [
            ("ascent", instance.ascent, ascent),
            ("descent", instance.descent, descent),
            ("leading", instance.leading, leading),
        ] {
            assert!(
                (ours * scale - core_text as f32).abs() < 0.01,
                "{}: {what} {} px, CoreText's {core_text}",
                style.name,
                ours * scale
            );
        }
        let (a, d, l) = (ascent as f32, descent as f32, leading as f32);
        let half = f64::from(l) * 0.5;
        // `fCorrectAscent`, `fCorrectDescent`, then
        // `InternalLineMetrics::height` and `correctedBaseline`.
        let correct_ascent = (f64::from(-a) - half) as f32;
        let correct_descent = (f64::from(d) + half) as f32;
        let height = (f64::from(correct_descent) - f64::from(correct_ascent)).round() as f32;
        let baseline = (f64::from(-correct_ascent) + 0.5).floor() as f32;
        (ascent, descent, leading, (height, baseline))
    };
    let laid = text::line_box(chosen.font, style.size, text::NATURAL);
    println!(
        "SkParagraph's line from CoreText's metrics ({ascent}, {descent}, {leading}): \
         {} px, baseline {}; cosmic-text's: {} px, baseline {}; the instance's: {:?}",
        sk_line.0,
        sk_line.1,
        laid.height,
        laid.baseline,
        chosen.line(style.size)
    );
    assert_eq!(
        (laid.height, laid.baseline),
        sk_line,
        "{}: the labels' line (height, baseline), cosmic-text's against SkParagraph's",
        style.name
    );
    assert_eq!(
        chosen.line(style.size),
        sk_line,
        "{}: the instance's line (height, baseline) against SkParagraph's",
        style.name
    );
    chosen
}

/// A font's family name.
#[cfg(target_os = "macos")]
fn family_of(font: &Font) -> &'static str {
    match font.family {
        Family::Name(name) => name,
        _ => "",
    }
}

/// On a Mac (CI's runner): every canvas label against the fonts CoreText
/// gave Skia ([`check_against_core_text`]).
#[cfg(target_os = "macos")]
#[test]
fn every_label_is_drawn_at_the_instance_and_on_the_line_coretext_gave_skia() {
    for style in &CANVAS_LABELS {
        check_against_core_text(style);
    }
}
