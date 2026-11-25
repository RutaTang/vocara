//! A variable font made up for the tests: an optical size axis along which
//! its advances, outlines and ascender vary, a weight axis, a width axis
//! if asked for, named instances if given, a line gap, a `STAT` table and a
//! `trak` table; and a font system of its own to lay it out in.

use std::sync::Arc;

use iced::advanced::graphics::text::cosmic_text::fontdb::{self, FaceInfo, Language, Source, ID};
use iced::advanced::graphics::text::cosmic_text::FontSystem;
use iced::advanced::text::{Alignment, Wrapping};
use iced::font::Family;
use iced::{Font, Size};

use crate::ui::core::widgets::studio_text::{lay_out, Spec};

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
pub const LETTERS: &str = "CutopyPase";
pub const UPEM: u16 = 1000;
/// Each glyph's advance at the default instance: 500 + 10 × its id.
pub fn advance(gid: u16) -> u16 {
    if gid == 0 {
        0
    } else {
        500 + 10 * gid
    }
}
/// How much wider each glyph and its advance are at the bottom of the
/// optical size axis (font units, scaled along it below its default).
pub const WIDER: i16 = 80;
pub const ASCENDER: i16 = 800;
pub const DESCENDER: i16 = -200;
pub const LINE_GAP: i16 = 40;
/// How much higher the ascender is at the bottom of the axis.
pub const HIGHER: i16 = 100;
/// The normal track at 12 pt, which harfrust would add to each glyph.
pub const TRACK_12: i16 = 50;
/// Its weight axis (min, default, max): San Francisco's.
pub const WGHT: (f32, f32, f32) = (1.0, 400.0, 1000.0);
/// Its width axis (min, default, max), when it has one.
pub const WDTH: (f32, f32, f32) = (75.0, 100.0, 125.0);

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

/// A made-up variable TrueType font mapping [`LETTERS`]: its axes `opsz`
/// (`opsz`: min, default, max), `wght` ([`WGHT`]) and, with `width`,
/// `wdth` ([`WDTH`]), in that order; below the default optical size each
/// glyph's box and advance grow by [`WIDER`] and the ascender by
/// [`HIGHER`] (`gvar`, `HVAR`, `MVAR`); its line gap is [`LINE_GAP`]; it
/// has a `STAT` table, a `trak` table whose normal track at 12 pt is
/// [`TRACK_12`], and `instances` for named instances (each one's user
/// coordinates, in axis order).
pub struct MadeUp {
    pub opsz: (f32, f32, f32),
    pub width: bool,
    pub instances: Vec<Vec<f32>>,
}

/// [`MadeUp`] with an `opsz` axis of `opsz`, no width axis and no named
/// instances.
pub fn variable_font(opsz: (f32, f32, f32)) -> Vec<u8> {
    MadeUp {
        opsz,
        width: false,
        instances: Vec::new(),
    }
    .build()
}

impl MadeUp {
    /// The font's bytes.
    pub fn build(&self) -> Vec<u8> {
        let glyphs = LETTERS.len() as u16 + 1;
        let axes = if self.width { 3 } else { 2 };
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
            variations.u16(11).u16(0xA000).f2dot14(-1.0);
            for _ in 1..axes {
                variations.f2dot14(0.0);
            }
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
        let instance_size = 4 + 4 * axes as u16;
        fvar.u16(1).u16(0).u16(16).u16(2).u16(axes as u16).u16(20);
        fvar.u16(self.instances.len() as u16).u16(instance_size);
        let mut axis = |tag: &[u8; 4], (min, default, max): (f32, f32, f32), name: u16| {
            fvar.tag(tag)
                .fixed(min)
                .fixed(default)
                .fixed(max)
                .u16(0)
                .u16(name);
        };
        axis(b"opsz", self.opsz, 256);
        axis(b"wght", WGHT, 257);
        if self.width {
            axis(b"wdth", WDTH, 258);
        }
        for (i, coords) in self.instances.iter().enumerate() {
            assert_eq!(coords.len(), axes, "a coordinate for each axis");
            fvar.u16(259 + i as u16).u16(0);
            for value in coords {
                fvar.fixed(*value);
            }
        }

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
}

/// The made-up font's family.
pub const FAMILY: &str = "Made Up UI";

/// A font system of nothing but `font` (in [`FAMILY`]), and its face.
pub fn font_system_of(font: Vec<u8>) -> (FontSystem, ID) {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    let id = fonts.db_mut().push_face_info(FaceInfo {
        id: ID::dummy(),
        source: Source::Binary(Arc::new(font)),
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

/// The made-up font in its own family, as cosmic-text draws it upstream.
pub fn made_up() -> Font {
    Font {
        family: Family::Name(FAMILY),
        ..Font::DEFAULT
    }
}

/// `text` laid out on one line in `font` at `size`, its line `line_height`
/// tall (zero: a `TextPainter`'s, the font's own), no letter spacing, with
/// tabular figures if `tabular`: each glyph's id and advance.
pub fn advances(
    fonts: &mut FontSystem,
    font: Font,
    size: f32,
    line_height: f32,
    tabular: bool,
    text: &str,
) -> Vec<(u16, f32)> {
    let spec = Spec {
        content: text,
        size,
        line_height,
        font,
        align_x: Alignment::Default,
        wrapping: Wrapping::None,
        letter_spacing: 0.0,
        tabular,
        max_lines: None,
    };
    lay_out(fonts, &spec, Size::INFINITE)
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| (g.glyph_id, g.w)))
        .collect()
}
