//! The labels' face: on every platform, Flutter's instance, line and
//! untracked shaping worked out on a variable font made up for the tests
//! (an optical size axis, advances, outlines and an ascender that vary
//! along it, a `STAT` and a `trak` table), registered in a font system of
//! its own; on a Mac, the system UI face against what CoreText gave Skia.

use std::sync::Arc;

use iced::advanced::graphics::text::cosmic_text::fontdb::{self, FaceInfo};
use iced::advanced::graphics::text::cosmic_text::skrifa::instance::Size as FontSize;
use iced::advanced::text::{Alignment, Wrapping};
use iced::Size;

use super::*;
use crate::ui::core::widgets::studio_text::{lay_out, Spec};

// ------------------------------------------------- a made-up variable font

/// Big-endian bytes.
#[derive(Default)]
struct Bytes(Vec<u8>);

impl Bytes {
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i16(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn tag(&mut self, tag: &[u8; 4]) -> &mut Self {
        self.0.extend_from_slice(tag);
        self
    }
    /// A 16.16 fixed-point number.
    fn fixed(&mut self, v: f32) -> &mut Self {
        self.u32((v * 65536.0).round() as i32 as u32)
    }
    /// A 2.14 fixed-point number.
    fn f2dot14(&mut self, v: f32) -> &mut Self {
        self.i16((v * 16384.0).round() as i16)
    }
}

/// The letters the made-up font maps, glyphs 1 on (`.notdef` is 0).
const LETTERS: &str = "CutopyPase";
const UPEM: u16 = 1000;
/// Each glyph's advance at the default instance: 500 + 10 × its id.
fn advance(gid: u16) -> u16 {
    if gid == 0 {
        0
    } else {
        500 + 10 * gid
    }
}
/// How much wider each glyph and its advance are at the bottom of the
/// optical size axis (font units, scaled along it below its default).
const WIDER: i16 = 80;
const ASCENDER: i16 = 800;
const DESCENDER: i16 = -200;
const LINE_GAP: i16 = 40;
/// How much higher the ascender is at the bottom of the axis.
const HIGHER: i16 = 100;
/// The normal track at 12 pt, which harfrust would add to each glyph.
const TRACK_12: i16 = 50;

/// A `trak` table with one horizontal set of `tracks`, each a track value
/// and its per-size values, over `sizes` (points).
fn trak_table(sizes: &[f32], tracks: &[(f32, &[i16])]) -> Vec<u8> {
    let entries_at = 12 + 8;
    let sizes_at = entries_at + 8 * tracks.len();
    let values_at = sizes_at + 4 * sizes.len();
    let mut out = Bytes::default();
    out.u32(0x0001_0000) // version 1.0
        .u16(0) // format
        .u16(12) // horizOffset
        .u16(0) // vertOffset: none
        .u16(0) // reserved
        .u16(tracks.len() as u16)
        .u16(sizes.len() as u16)
        .u32(sizes_at as u32);
    for (i, (track, _)) in tracks.iter().enumerate() {
        let at = values_at + 2 * sizes.len() * i;
        out.fixed(*track).u16(256 + i as u16).u16(at as u16);
    }
    for size in sizes {
        out.fixed(*size);
    }
    for (_, values) in tracks {
        for value in *values {
            out.i16(*value);
        }
    }
    out.0
}

/// An item variation store of one region, below the default of the
/// `axis`-th of `axes` axes (peak -1), with one delta set: `deltas`.
fn item_variation_store(axes: usize, axis: usize, deltas: &[i8]) -> Vec<u8> {
    let regions_at = 12;
    let regions_len = 4 + 6 * axes;
    let data_at = regions_at + regions_len;
    let mut out = Bytes::default();
    out.u16(1) // format
        .u32(regions_at as u32)
        .u16(1) // itemVariationDataCount
        .u32(data_at as u32);
    out.u16(axes as u16).u16(1);
    for a in 0..axes {
        if a == axis {
            out.f2dot14(-1.0).f2dot14(-1.0).f2dot14(0.0);
        } else {
            out.f2dot14(0.0).f2dot14(0.0).f2dot14(0.0);
        }
    }
    out.u16(deltas.len() as u16) // itemCount
        .u16(0) // wordDeltaCount: bytes
        .u16(1) // regionIndexCount
        .u16(0);
    for delta in deltas {
        out.0.push(*delta as u8);
    }
    out.0
}

/// A variable TrueType font made up for the tests, mapping [`LETTERS`],
/// with an `opsz` axis of `opsz` (min, default, max) and a `wght` axis
/// (100, 400, 900); below the default optical size each glyph's box and
/// advance grow by [`WIDER`] and the ascender by [`HIGHER`] (`gvar`,
/// `HVAR`, `MVAR`); it has a `STAT` table, and a `trak` table whose
/// normal track at 12 pt is [`TRACK_12`].
fn variable_font(opsz: (f32, f32, f32)) -> Vec<u8> {
    let glyphs = LETTERS.len() as u16 + 1;
    let axes = 2;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();

    // Outlines: a box from x 50 to its advance less 50, 700 high.
    let mut glyf = Bytes::default();
    let mut loca = Bytes::default();
    for gid in 0..glyphs {
        loca.u32(glyf.0.len() as u32);
        if gid == 0 {
            continue;
        }
        let right = advance(gid) as i16 - 50;
        glyf.i16(1).i16(50).i16(0).i16(right).i16(700);
        glyf.u16(3).u16(0); // endPtsOfContours, no instructions
        glyf.0.extend_from_slice(&[1, 1, 1, 1]); // on-curve, long x and y
        for dx in [50, 0, right - 50, 0] {
            glyf.i16(dx);
        }
        for dy in [0, 700, 0, -700] {
            glyf.i16(dy);
        }
    }
    loca.u32(glyf.0.len() as u32);

    let mut gvar = Bytes::default();
    let offsets_at = 20;
    let data_at = offsets_at + 4 * (usize::from(glyphs) + 1);
    gvar.u16(1)
        .u16(0)
        .u16(axes as u16)
        .u16(0)
        .u32(data_at as u32);
    gvar.u16(glyphs).u16(1).u32(data_at as u32);
    let mut variations = Bytes::default();
    for gid in 0..glyphs {
        gvar.u32(variations.0.len() as u32);
        if gid == 0 {
            continue;
        }
        // One tuple, peak -1 on opsz (embedded, its own point numbers),
        // with deltas for every point: the box's right side and the
        // advance's phantom point move right.
        variations.u16(1).u16(4 + 4 + 2 * axes as u16);
        variations.u16(11).u16(0xA000).f2dot14(-1.0).f2dot14(0.0);
        variations.0.push(0); // no point numbers: all of them
        variations.0.push(0x07); // 8 x deltas as bytes
        for dx in [0, 0, WIDER, WIDER, 0, WIDER, 0, 0] {
            variations.0.push(dx as i8 as u8);
        }
        variations.0.push(0x87); // 8 zero y deltas
    }
    gvar.u32(variations.0.len() as u32);
    gvar.0.extend_from_slice(&variations.0);

    let mut hvar = Bytes::default();
    hvar.u16(1).u16(0).u32(20).u32(0).u32(0).u32(0);
    let deltas: Vec<i8> = (0..glyphs)
        .map(|gid| if gid == 0 { 0 } else { WIDER as i8 })
        .collect();
    hvar.0.extend(item_variation_store(axes, 0, &deltas));

    let mut mvar = Bytes::default();
    mvar.u16(1).u16(0).u16(0).u16(8).u16(1).u16(20);
    mvar.tag(b"hasc").u16(0).u16(0);
    mvar.0
        .extend(item_variation_store(axes, 0, &[HIGHER as i8]));

    let mut fvar = Bytes::default();
    fvar.u16(1).u16(0).u16(16).u16(2).u16(axes as u16).u16(20);
    fvar.u16(0).u16(4 + 4 * axes as u16);
    fvar.tag(b"opsz")
        .fixed(opsz.0)
        .fixed(opsz.1)
        .fixed(opsz.2)
        .u16(0)
        .u16(256);
    fvar.tag(b"wght")
        .fixed(100.0)
        .fixed(400.0)
        .fixed(900.0)
        .u16(0)
        .u16(257);

    let mut stat = Bytes::default();
    stat.u16(1).u16(1).u16(8).u16(0).u32(0).u16(0).u32(0).u16(2);

    let mut cmap = Bytes::default();
    let mut letters: Vec<(char, u16)> = LETTERS.chars().zip(1..).collect();
    letters.sort();
    cmap.u16(0).u16(1).u16(3).u16(10).u32(12);
    cmap.u16(12)
        .u16(0)
        .u32(16 + 12 * letters.len() as u32)
        .u32(0);
    cmap.u32(letters.len() as u32);
    for (ch, gid) in &letters {
        cmap.u32(*ch as u32).u32(*ch as u32).u32(u32::from(*gid));
    }

    let mut hmtx = Bytes::default();
    for gid in 0..glyphs {
        hmtx.u16(advance(gid)).i16(if gid == 0 { 0 } else { 50 });
    }

    let mut head = Bytes::default();
    head.u16(1)
        .u16(0)
        .fixed(1.0)
        .u32(0)
        .u32(0x5F0F_3CF5)
        .u16(3)
        .u16(UPEM);
    head.u32(0).u32(0).u32(0).u32(0); // created, modified
    head.i16(0).i16(0).i16(700).i16(700);
    head.u16(0).u16(8).i16(2).i16(1).i16(0); // long loca

    let mut hhea = Bytes::default();
    hhea.u16(1)
        .u16(0)
        .i16(ASCENDER)
        .i16(DESCENDER)
        .i16(LINE_GAP);
    hhea.u16(advance(glyphs - 1) + WIDER as u16)
        .i16(0)
        .i16(0)
        .i16(700);
    hhea.i16(1).i16(0).i16(0).i16(0).i16(0).i16(0).i16(0).i16(0);
    hhea.u16(glyphs);

    let mut maxp = Bytes::default();
    maxp.u32(0x0001_0000).u16(glyphs).u16(4).u16(1);
    maxp.u16(0).u16(0).u16(2);
    for _ in 0..8 {
        maxp.u16(0);
    }

    let trak = trak_table(
        &[9.0, 12.0, 17.0],
        &[(0.0, &[TRACK_12 + 10, TRACK_12, TRACK_12 - 30])],
    );

    tables.push((*b"HVAR", hvar.0));
    tables.push((*b"MVAR", mvar.0));
    tables.push((*b"STAT", stat.0));
    tables.push((*b"cmap", cmap.0));
    tables.push((*b"fvar", fvar.0));
    tables.push((*b"glyf", glyf.0));
    tables.push((*b"gvar", gvar.0));
    tables.push((*b"head", head.0));
    tables.push((*b"hhea", hhea.0));
    tables.push((*b"hmtx", hmtx.0));
    tables.push((*b"loca", loca.0));
    tables.push((*b"maxp", maxp.0));
    tables.push((*b"trak", trak));

    let mut font = Bytes::default();
    let count = tables.len() as u16;
    font.u32(0x0001_0000)
        .u16(count)
        .u16(128)
        .u16(3)
        .u16(count * 16 - 128);
    let mut at = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        font.tag(tag).u32(0).u32(at as u32).u32(data.len() as u32);
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        at = 12 + 16 * tables.len() + body.len();
    }
    font.0.extend(body);
    font.0
}

/// The made-up font's family.
const FAMILY: &str = "Made Up UI";

/// A font system of the made-up font with an `opsz` axis of `opsz`, and
/// its face.
fn font_system_of(opsz: (f32, f32, f32)) -> (FontSystem, ID) {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    let id = fonts.db_mut().push_face_info(FaceInfo {
        id: ID::dummy(),
        source: Source::Binary(Arc::new(variable_font(opsz))),
        index: 0,
        families: vec![(FAMILY.to_string(), Language::English_UnitedStates)],
        post_script_name: "MadeUpUI".to_string(),
        style: fontdb::Style::Normal,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        monospaced: false,
    });
    (fonts, id)
}

/// `text` laid out on one line in `font` at the labels' size, no letter
/// spacing: each glyph's id and advance.
fn advances(fonts: &mut FontSystem, font: Font, text: &str) -> Vec<(u16, f32)> {
    let spec = Spec {
        content: text,
        size: LABEL_SIZE,
        line_height: 20.0,
        font,
        align_x: Alignment::Default,
        wrapping: Wrapping::None,
        letter_spacing: 0.0,
        tabular: false,
        max_lines: None,
    };
    lay_out(fonts, &spec, Size::INFINITE)
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| (g.glyph_id, g.w)))
        .collect()
}

/// The made-up font in its own family, as cosmic-text draws it upstream.
fn made_up() -> Font {
    Font {
        family: Family::Name(FAMILY),
        ..Font::DEFAULT
    }
}

#[test]
fn flutters_instance_is_the_12_pt_optical_size_at_weight_400_clamped_to_the_axes() {
    let instance = |opsz| {
        let data = variable_font(opsz);
        Instance::of(&FontRef::new(&data).expect("the made-up font parses")).expect("an instance")
    };
    // The axis reaches down to 12: Skia's 12 pt optical size, not the
    // 14 px CoreText would pick by itself, nor the default.
    let low = instance((6.0, 28.0, 96.0));
    assert_eq!(low.coord(b"opsz"), Some(12.0));
    assert_eq!(low.coord(b"wght"), Some(400.0));
    assert_eq!(low.coord(b"wdth"), None);
    // It starts above 12: its lowest.
    assert_eq!(instance((17.0, 28.0, 96.0)).coord(b"opsz"), Some(17.0));
    // A face without the axes keeps none.
    let inter = FontRef::new(font::FACES[0]).expect("Inter parses");
    assert!(Instance::of(&inter).expect("Inter's").coords.is_empty());
}

#[test]
fn the_line_is_skparagraphs_from_the_hhea_metrics_and_their_mvar_deltas_there() {
    let data = variable_font((6.0, 28.0, 96.0));
    let instance = Instance::of(&FontRef::new(&data).expect("parses")).expect("an instance");
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
    let (mut fonts, id) = font_system_of((6.0, 28.0, 96.0));
    let (face, instance) = register(&mut fonts, id).expect("registered");
    assert!(face.system);
    assert_eq!(face.font.family, Family::Name(LABEL_FAMILY));
    assert_eq!(
        fonts.face_instance(id),
        None,
        "the face itself is untouched"
    );
    let data = variable_font((6.0, 28.0, 96.0));
    let font = FontRef::new(&data).expect("parses");
    let location = font.axes().location(instance.coords.iter().copied());
    let metrics = font.glyph_metrics(FontSize::new(LABEL_SIZE), &location);
    let scale = LABEL_SIZE / f32::from(UPEM);
    for label in ["Cut", "Copy", "Paste"] {
        let drawn = advances(&mut fonts, face.font, label);
        let upstream = advances(&mut fonts, made_up(), label);
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
fn the_registered_face_is_drawn_at_the_instance() {
    use iced::advanced::graphics::text::cosmic_text::SwashCache;
    let (mut fonts, id) = font_system_of((6.0, 28.0, 96.0));
    let (face, _) = register(&mut fonts, id).expect("registered");
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

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_labels_are_in_inter_on_skparagraphs_line() {
    assert_eq!(label_face(), &inter());
    assert_eq!(label_face().font, font::REGULAR);
    let inter =
        Instance::of(&FontRef::new(font::FACES[0]).expect("Inter parses")).expect("Inter's");
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

// ------------------------------------------------------------ on a Mac

/// CoreText and CoreFoundation, as much as the test asks them.
#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals, non_snake_case, dead_code)]
mod core_text {
    use std::ffi::{c_char, c_void, CString};

    pub type CFTypeRef = *const c_void;

    #[repr(C)]
    pub struct Opaque {
        _private: [u8; 0],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }

    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;
    const SINT32: isize = 3;
    const FLOAT64: isize = 13;
    /// `kCTFontUIFontSystem`.
    pub const UI_FONT_SYSTEM: u32 = 2;
    /// `kCTFontOrientationHorizontal`.
    pub const HORIZONTAL: u32 = 1;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Opaque;
        static kCFTypeDictionaryValueCallBacks: Opaque;
        static kCFTypeSetCallBacks: Opaque;
        fn CFDictionaryCreateMutable(
            allocator: CFTypeRef,
            capacity: isize,
            keys: *const Opaque,
            values: *const Opaque,
        ) -> *mut c_void;
        fn CFDictionarySetValue(dict: *mut c_void, key: CFTypeRef, value: CFTypeRef);
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFDictionaryGetCount(dict: CFTypeRef) -> isize;
        fn CFDictionaryGetKeysAndValues(
            dict: CFTypeRef,
            keys: *mut CFTypeRef,
            values: *mut CFTypeRef,
        );
        fn CFStringCreateWithCString(
            allocator: CFTypeRef,
            s: *const c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
        fn CFNumberGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFSetCreate(
            allocator: CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            callbacks: *const Opaque,
        ) -> CFTypeRef;
        fn CFURLCopyFileSystemPath(url: CFTypeRef, style: isize) -> CFTypeRef;
    }

    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        static kCTFontFamilyNameAttribute: CFTypeRef;
        static kCTFontTraitsAttribute: CFTypeRef;
        static kCTFontWeightTrait: CFTypeRef;
        static kCTFontWidthTrait: CFTypeRef;
        static kCTFontSlantTrait: CFTypeRef;
        static kCTFontURLAttribute: CFTypeRef;
        fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: CFTypeRef) -> CFTypeRef;
        fn CTFontCopyFamilyName(font: CFTypeRef) -> CFTypeRef;
        fn CTFontDescriptorCreateWithAttributes(attributes: CFTypeRef) -> CFTypeRef;
        fn CTFontDescriptorCreateMatchingFontDescriptors(
            descriptor: CFTypeRef,
            mandatory: CFTypeRef,
        ) -> CFTypeRef;
        fn CTFontDescriptorCopyAttribute(descriptor: CFTypeRef, attribute: CFTypeRef) -> CFTypeRef;
        fn CTFontCreateWithFontDescriptor(
            descriptor: CFTypeRef,
            size: f64,
            matrix: *const c_void,
        ) -> CFTypeRef;
        fn CTFontCopyAttribute(font: CFTypeRef, attribute: CFTypeRef) -> CFTypeRef;
        fn CTFontGetSize(font: CFTypeRef) -> f64;
        fn CTFontCreateCopyWithAttributes(
            font: CFTypeRef,
            size: f64,
            matrix: *const c_void,
            attributes: CFTypeRef,
        ) -> CFTypeRef;
        fn CTFontCopyVariation(font: CFTypeRef) -> CFTypeRef;
        fn CTFontGetAdvancesForGlyphs(
            font: CFTypeRef,
            orientation: u32,
            glyphs: *const u16,
            advances: *mut CGSize,
            count: isize,
        ) -> f64;
        fn CTFontGetAscent(font: CFTypeRef) -> f64;
        fn CTFontGetDescent(font: CFTypeRef) -> f64;
        fn CTFontGetLeading(font: CFTypeRef) -> f64;
    }

    fn string(s: &str) -> CFTypeRef {
        let s = CString::new(s).expect("no NUL");
        // SAFETY: a valid C string; the result is a CFString (leaked).
        unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) }
    }

    fn to_string(s: CFTypeRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let mut buffer = vec![0 as c_char; 1024];
        // SAFETY: `s` is a CFString; the buffer is as long as said.
        let ok = unsafe { CFStringGetCString(s, buffer.as_mut_ptr(), 1024, UTF8) };
        (ok != 0).then(|| {
            // SAFETY: CFStringGetCString wrote a NUL-terminated string.
            unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }

    fn f64_number(value: f64) -> CFTypeRef {
        // SAFETY: a pointer to an f64 for a float64 number.
        unsafe { CFNumberCreate(std::ptr::null(), FLOAT64, (&raw const value).cast()) }
    }

    fn i32_number(value: i32) -> CFTypeRef {
        // SAFETY: a pointer to an i32 for an sint32 number.
        unsafe { CFNumberCreate(std::ptr::null(), SINT32, (&raw const value).cast()) }
    }

    fn number(value: CFTypeRef) -> Option<f64> {
        // SAFETY: only a CFNumber is read.
        unsafe {
            if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
                return None;
            }
            let mut out = 0.0_f64;
            (CFNumberGetValue(value, FLOAT64, (&raw mut out).cast()) != 0).then_some(out)
        }
    }

    fn integer(value: CFTypeRef) -> Option<i64> {
        // SAFETY: only a CFNumber is read.
        unsafe {
            if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
                return None;
            }
            let mut out = 0_i64;
            (CFNumberGetValue(value, SINT64, (&raw mut out).cast()) != 0).then_some(out)
        }
    }

    fn dictionary() -> *mut c_void {
        // SAFETY: the CFType callbacks CoreFoundation exports.
        unsafe {
            CFDictionaryCreateMutable(
                std::ptr::null(),
                0,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        }
    }

    /// A CoreText font (leaked: the test's).
    #[derive(Clone, Copy)]
    pub struct CtFont(CFTypeRef);

    /// The family `[NSFont systemFontOfSize:size]` has (that font is
    /// CoreText's system UI font at the size).
    pub fn system_font_family(size: f64) -> String {
        // SAFETY: CoreText calls on what they return.
        unsafe {
            let font = CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, size, std::ptr::null());
            assert!(!font.is_null(), "the system UI font");
            to_string(CTFontCopyFamilyName(font)).expect("its family name")
        }
    }

    /// Skia's typeface of `family`'s regular style
    /// (`SkFontStyleSet_Mac::matchStyle`, `create_from_desc`): the
    /// family's descriptors matched by name, the regular one (CoreText's
    /// weight, width and slant traits 0: CSS 400, normal, upright) made a
    /// font at size 0.
    pub fn skia_typeface(family: &str) -> CtFont {
        // SAFETY: CoreFoundation and CoreText calls on what they return.
        unsafe {
            let attributes = dictionary();
            CFDictionarySetValue(attributes, kCTFontFamilyNameAttribute, string(family));
            let descriptor = CTFontDescriptorCreateWithAttributes(attributes);
            let mandatory = [kCTFontFamilyNameAttribute];
            let mandatory = CFSetCreate(
                std::ptr::null(),
                mandatory.as_ptr(),
                1,
                &raw const kCFTypeSetCallBacks,
            );
            let matches = CTFontDescriptorCreateMatchingFontDescriptors(descriptor, mandatory);
            assert!(!matches.is_null(), "{family} matches descriptors");
            let trait_of = |descriptor: CFTypeRef, name: CFTypeRef| {
                let traits = CTFontDescriptorCopyAttribute(descriptor, kCTFontTraitsAttribute);
                if traits.is_null() {
                    return 0.0;
                }
                number(CFDictionaryGetValue(traits, name)).unwrap_or(0.0)
            };
            let regular = (0..CFArrayGetCount(matches))
                .map(|i| CFArrayGetValueAtIndex(matches, i))
                .min_by(|a, b| {
                    let off = |d: CFTypeRef| {
                        trait_of(d, kCTFontWeightTrait).abs()
                            + trait_of(d, kCTFontWidthTrait).abs()
                            + trait_of(d, kCTFontSlantTrait).abs() * 10.0
                    };
                    off(*a).total_cmp(&off(*b))
                })
                .expect("a descriptor of the family");
            CtFont(CTFontCreateWithFontDescriptor(
                regular,
                0.0,
                std::ptr::null(),
            ))
        }
    }

    impl CtFont {
        pub fn size(self) -> f64 {
            // SAFETY: a CTFont.
            unsafe { CTFontGetSize(self.0) }
        }

        /// Its `NSCTFontOpticalSizeAttribute`, if a number.
        pub fn optical_size_attribute(self) -> Option<f64> {
            // SAFETY: a CTFont; the attribute is only read as a number.
            unsafe {
                let value = CTFontCopyAttribute(self.0, string("NSCTFontOpticalSizeAttribute"));
                if !value.is_null() && CFGetTypeID(value) == CFStringGetTypeID() {
                    println!("NSCTFontOpticalSizeAttribute: {:?}", to_string(value));
                }
                number(value)
            }
        }

        /// `SkCTFontCreateExactCopy` of it at `size` (no `OpszVariation`):
        /// the optical size its attribute's, else its size; no tracking.
        /// And that optical size.
        pub fn skia_exact_copy(self, size: f64) -> (CtFont, f64) {
            let opsz = self
                .optical_size_attribute()
                .filter(|v| *v > 0.0)
                .unwrap_or_else(|| self.size());
            // SAFETY: CoreFoundation and CoreText calls on what they
            // return.
            unsafe {
                let attributes = dictionary();
                CFDictionarySetValue(
                    attributes,
                    string("NSCTFontOpticalSizeAttribute"),
                    f64_number(opsz),
                );
                CFDictionarySetValue(
                    attributes,
                    string("NSCTFontUnscaledTrackingAttribute"),
                    i32_number(0),
                );
                let descriptor = CTFontDescriptorCreateWithAttributes(attributes);
                let copy =
                    CTFontCreateCopyWithAttributes(self.0, size, std::ptr::null(), descriptor);
                assert!(!copy.is_null(), "the exact copy");
                (CtFont(copy), opsz)
            }
        }

        /// The file it is drawn from.
        pub fn path(self) -> Option<String> {
            // SAFETY: a CTFont; its URL attribute is a CFURL.
            unsafe {
                let url = CTFontCopyAttribute(self.0, kCTFontURLAttribute);
                if url.is_null() {
                    return None;
                }
                to_string(CFURLCopyFileSystemPath(url, 0))
            }
        }

        /// Its variation (`CTFontCopyVariation`: the axes not at their
        /// defaults), by axis tag.
        pub fn variation(self) -> Vec<(u32, f64)> {
            // SAFETY: a CTFont; the dictionary's keys and values are read
            // as numbers.
            unsafe {
                let variation = CTFontCopyVariation(self.0);
                if variation.is_null() {
                    return Vec::new();
                }
                let count = CFDictionaryGetCount(variation).max(0) as usize;
                let mut keys = vec![std::ptr::null(); count];
                let mut values = vec![std::ptr::null(); count];
                CFDictionaryGetKeysAndValues(variation, keys.as_mut_ptr(), values.as_mut_ptr());
                keys.iter()
                    .zip(&values)
                    .filter_map(|(k, v)| Some((integer(*k)? as u32, number(*v)?)))
                    .collect()
            }
        }

        /// The advance of glyph `gid`, in points.
        pub fn advance(self, gid: u16) -> f64 {
            let mut advance = CGSize::default();
            // SAFETY: a CTFont, one glyph and one advance.
            unsafe {
                CTFontGetAdvancesForGlyphs(self.0, HORIZONTAL, &gid, &mut advance, 1);
            }
            advance.width
        }

        /// Its ascent, descent and leading, in points.
        pub fn metrics(self) -> (f64, f64, f64) {
            // SAFETY: a CTFont.
            unsafe {
                (
                    CTFontGetAscent(self.0),
                    CTFontGetDescent(self.0),
                    CTFontGetLeading(self.0),
                )
            }
        }
    }
}

/// On a Mac (CI's runner): asks CoreText for the fonts Skia made for the
/// labels (its typeface of `.AppleSystemUIFont`, its exact copy at 14 pt)
/// and checks the labels against them: the face (the same file), the
/// instance (every axis at the exact copy's variation), each glyph's
/// advance, the positioning HarfBuzz applied at the typeface's own
/// variation, and the line (CoreText's ascent, descent and leading).
#[cfg(target_os = "macos")]
#[test]
fn the_labels_are_drawn_at_the_instance_and_metrics_coretext_gave_skia() {
    use iced::advanced::graphics::text::font_system;

    use super::super::toolbar::{item_height, Item, LABEL_SIZE};

    // Chosen first: it takes the font system's lock.
    let chosen = label_face().clone();
    println!("labels' face: {chosen:?}");
    assert!(
        chosen.system,
        "on a Mac the labels are in the system UI font"
    );

    let family = core_text::system_font_family(14.0);
    println!("[NSFont systemFontOfSize:14].familyName: {family}");
    let typeface = core_text::skia_typeface(&family);
    println!(
        "Skia's typeface: {} pt, variation {:?}",
        typeface.size(),
        typeface.variation()
    );
    let (copy, opsz) = typeface.skia_exact_copy(f64::from(LABEL_SIZE));
    let path = copy.path().unwrap_or_default();
    println!(
        "Skia's exact copy at {LABEL_SIZE} pt: optical size {opsz}, {path}, variation {:?}",
        copy.variation()
    );
    assert!(
        path.ends_with("SFNS.ttf"),
        "CoreText draws {family} from {path}"
    );

    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    let id = system_ui_face(fonts.db()).expect("SFNS.ttf loaded from /System/Library/Fonts");
    let data_of = |fonts: &FontSystem| {
        fonts
            .db()
            .with_face_data(id, |data, index| (data.to_vec(), index))
            .expect("the face's data")
    };
    let (data, index) = data_of(fonts);
    let face = FontRef::from_index(&data, index).expect("SFNS.ttf parses");
    let instance = Instance::of(&face).expect("an instance");
    let axes: Vec<(Tag, f32, f32, f32)> = face
        .axes()
        .iter()
        .map(|a| (a.tag(), a.min_value(), a.default_value(), a.max_value()))
        .collect();
    println!("axes (min, default, max): {axes:?}");
    println!("the labels' instance: {instance:?}");

    // Every axis where CoreText set the exact copy's (an axis it does not
    // report is at its default).
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
            "{tag}: the labels' {ours}, CoreText's {core_text}"
        );
    }
    // HarfBuzz positioned the glyphs at the typeface's own variation.
    let typeface_variation = typeface.variation();
    let shaped_at: Vec<(Tag, f32)> = axes
        .iter()
        .map(|(tag, _, default, _)| (*tag, at(&typeface_variation, *tag, *default)))
        .collect();
    println!("HarfBuzz's variation: {shaped_at:?}");
    let same = shaped_at.iter().all(|(tag, v)| {
        instance
            .coord(&tag.to_be_bytes())
            .is_some_and(|ours| (ours - v).abs() < 0.01)
    });
    // A copy of the face shaped there, to compare its positioning with.
    let harfbuzz = if same {
        None
    } else {
        let info = fonts.db().face(id).expect("the face").clone();
        let copy_id = fonts.db_mut().push_face_info(FaceInfo {
            families: vec![(
                ".Vocara HarfBuzz Variation".into(),
                Language::English_UnitedStates,
            )],
            ..info
        });
        fonts.set_face_instance(
            copy_id,
            Some(FaceInstance {
                coords: shaped_at
                    .iter()
                    .map(|(t, v)| (t.to_be_bytes(), *v))
                    .collect(),
                untracked: true,
            }),
        );
        Some(Font {
            family: Family::Name(".Vocara HarfBuzz Variation"),
            ..Font::DEFAULT
        })
    };

    let size = FontSize::new(LABEL_SIZE);
    let ours_at = face.axes().location(instance.coords.iter().copied());
    let harfbuzz_at = face.axes().location(shaped_at.iter().copied());
    let nominal = |location, gid: u16| {
        face.glyph_metrics(size, location)
            .advance_width(gid.into())
            .expect("an advance")
    };
    for item in [Item::Cut, Item::Copy, Item::Paste] {
        let label = item.label();
        let drawn = advances(fonts, chosen.font, label);
        assert_eq!(drawn.len(), label.chars().count(), "{label}: {drawn:?}");
        let positioned = harfbuzz.map(|font| advances(fonts, font, label));
        for (i, (gid, w)) in drawn.iter().enumerate() {
            // The instance's advance is CoreText's.
            let ours = nominal(&ours_at, *gid);
            let core_text = copy.advance(*gid) as f32;
            assert!(
                (ours - core_text).abs() < 0.01,
                "{label} glyph {gid}: advance {ours}, CoreText's {core_text}"
            );
            // What the shaper added to it, HarfBuzz's at its variation.
            let added = w - ours;
            if let Some(positioned) = &positioned {
                let (hb_gid, hb_w) = positioned[i];
                assert_eq!(hb_gid, *gid, "{label}: the same glyphs");
                let hb_added = hb_w - nominal(&harfbuzz_at, hb_gid);
                assert!(
                    (added - hb_added).abs() < 0.01,
                    "{label} glyph {gid}: positioned {added} past its advance, HarfBuzz {hb_added}"
                );
            }
            println!("{label} glyph {gid}: {w} px ({ours} + {added})");
        }
    }

    // The line: CoreText's metrics of the exact copy.
    let (ascent, descent, leading) = copy.metrics();
    let scale = LABEL_SIZE / f32::from(instance.units_per_em);
    println!(
        "CoreText's ascent, descent, leading at {LABEL_SIZE} pt: {ascent} {descent} {leading}; \
         the labels' {} {} {}",
        instance.ascent * scale,
        instance.descent * scale,
        instance.leading * scale
    );
    for (ours, core_text) in [
        (instance.ascent, ascent),
        (instance.descent, descent),
        (instance.leading, leading),
    ] {
        assert!(
            (ours * scale - core_text as f32).abs() < 0.01,
            "{ours} {core_text}"
        );
    }
    let height = (ascent + descent + leading).round() as f32;
    assert_eq!(chosen.line_height, height);
    assert_eq!(
        chosen.baseline,
        (leading / 2.0 + ascent + 0.5).floor() as f32
    );
    assert_eq!(item_height(), 2.0 + height + 5.0);
}
