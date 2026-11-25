//! The face the text toolbar's labels are set in ([`label_face`]): the
//! macOS system UI font where iced's text engine draws it as Flutter drew
//! the labels, else the bundled Inter.
//!
//! Flutter's label style set no family (`inherit: false`), so the labels
//! were in the platform's default, `.AppleSystemUIFont`: SF, which macOS
//! (10.15 on) ships as one variable face, `/System/Library/Fonts/SFNS.ttf`,
//! under a hidden family (`.SF NS`) the font system loads with the other
//! system fonts. Whether cosmic-text 0.15 (iced's text engine) draws that
//! face as Flutter did depends on the face, so [`SystemUiFace`] reads it,
//! once, and the labels take it only when all of these hold:
//!
//! - **design**: cosmic-text varies a face along its weight axis alone (its
//!   shaper's instance and its rasterizer's), so every other axis stays at
//!   its default. CoreText, which drew Flutter's glyphs, sets the optical
//!   size (`opsz`) from the size in points, clamped to the axis: the
//!   label's 14 pt, or 12 pt, CoreText's default size, at which Skia made
//!   the face and whose optical size it keeps. The outlines and advances
//!   are the same only where the axis's default is where both clamp to;
//!   a face without the axis has nothing to set;
//! - **tracking**: Flutter's shaper (HarfBuzz) and cosmic-text's (harfrust)
//!   both apply a face's `trak` table, at the size they are given, else
//!   12 pt (harfrust, and HarfBuzz since 2025; HarfBuzz before applied none
//!   then), and only to a face with a `STAT` table. Neither is given one.
//!   The normal track at 12 pt must be 0, as Apple's tracking is at 12 pt,
//!   for every one of them to agree; the -0.15 px letter spacing on top is
//!   the toolbar style's own;
//! - **line box**: the labels' line is the face's ascent and descent (its
//!   `hhea`, as CoreText reports them), in place of Inter's 1.21 em:
//!   Flutter's line had no `height`, so its box was its font's.
//!
//! Off macOS, without the face, or when a check fails, the labels stay in
//! Inter at its natural line box. On a Mac (CI's runner), the test
//! `the_labels_take_the_system_font_exactly_when_cosmic_text_draws_it_as_flutter_did`
//! reads the face, prints what it found, and checks each step against what
//! cosmic-text does with it.

use std::sync::OnceLock;

use iced::advanced::graphics::text::cosmic_text::fontdb::{Database, Source, ID};
use iced::advanced::graphics::text::cosmic_text::skrifa::{
    raw::{tables::trak::Trak, TableProvider},
    FontRef, MetadataProvider, Tag,
};
use iced::Font;

use crate::ui::core::theme::font;

use super::toolbar::{LABEL_LINE_HEIGHT, LABEL_SIZE};

/// Where macOS keeps its system UI font.
pub const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";

/// The size HarfBuzz and harfrust track a face at when given none
/// (CoreText's default size), which cosmic-text never gives its shaper;
/// and the size Skia made Flutter's system font face at.
pub const DEFAULT_PT: f32 = 12.0;

/// The labels' face and line box.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelFace {
    pub font: Font,
    /// The line box, in ems of the label size.
    pub line_height: f32,
    /// Whether it is the system UI font (else Inter).
    pub system: bool,
}

/// The labels' face (see the module docs), chosen the first time it is
/// asked for. Takes the font system's lock the first time: never call it
/// with that lock held.
pub fn label_face() -> &'static LabelFace {
    static FACE: OnceLock<LabelFace> = OnceLock::new();
    FACE.get_or_init(|| choose(LABEL_SIZE))
}

/// Inter at its natural line box.
fn inter() -> LabelFace {
    LabelFace {
        font: font::REGULAR,
        line_height: LABEL_LINE_HEIGHT,
        system: false,
    }
}

#[cfg(target_os = "macos")]
fn choose(pt: f32) -> LabelFace {
    use iced::advanced::graphics::text::font_system;
    use iced::font::{Family, Stretch, Style, Weight};
    let face = {
        let mut fonts = font_system().write().expect("the font system");
        SystemUiFace::read(fonts.raw().db())
    };
    match face.filter(|face| face.draws_as_flutter(pt)) {
        Some(face) => LabelFace {
            font: Font {
                // Once per process: the family outlives every text.
                family: Family::Name(Box::leak(face.family.into_boxed_str())),
                weight: Weight::Normal,
                stretch: Stretch::Normal,
                style: Style::Normal,
            },
            line_height: face.line_height,
            system: true,
        },
        None => inter(),
    }
}

#[cfg(not(target_os = "macos"))]
fn choose(_pt: f32) -> LabelFace {
    inter()
}

/// One of a variable face's axes, in user units.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    pub tag: String,
    pub min: f32,
    pub default: f32,
    pub max: f32,
}

/// What the choice reads off the system UI face.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemUiFace {
    /// The face in the font system's database.
    pub id: ID,
    /// Its (hidden) family name.
    pub family: String,
    /// Its variation axes (`fvar`), in order.
    pub axes: Vec<Axis>,
    pub units_per_em: u16,
    /// `hhea`'s ascender + |descender|, in ems (its line gap is not part of
    /// a line's box).
    pub line_height: f32,
    /// What cosmic-text's shaper adds to each glyph's advance, in font
    /// units: the `trak` table's normal track at [`DEFAULT_PT`], rounded
    /// (harfrust applies it only with a `STAT` table too); 0 without.
    pub default_tracking: f32,
    /// The normal track at 14 pt, in font units: CoreText's at the label
    /// size (for the record; Flutter did not shape with CoreText).
    pub label_tracking: f32,
}

impl SystemUiFace {
    /// Reads the face loaded from [`SYSTEM_UI_FONT`], if the font system
    /// has one.
    pub fn read(db: &Database) -> Option<SystemUiFace> {
        let face = db.faces().find(|face| match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => {
                // However the font system spelled the folder it loaded.
                path.ends_with(SYSTEM_UI_FONT.trim_start_matches('/'))
            }
            Source::Binary(_) => false,
        })?;
        let family = face.families.first()?.0.clone();
        let id = face.id;
        db.with_face_data(id, |data, index| {
            Self::parse(FontRef::from_index(data, index).ok()?, id, family)
        })?
    }

    fn parse(font: FontRef<'_>, id: ID, family: String) -> Option<SystemUiFace> {
        let axes = font
            .axes()
            .iter()
            .map(|axis| Axis {
                tag: axis.tag().to_string(),
                min: axis.min_value(),
                default: axis.default_value(),
                max: axis.max_value(),
            })
            .collect();
        let units_per_em = font.head().ok()?.units_per_em();
        let hhea = font.hhea().ok()?;
        let line_height = (f32::from(hhea.ascender().to_i16())
            - f32::from(hhea.descender().to_i16()))
            / f32::from(units_per_em.max(1));
        let trak = font.trak().ok();
        let applied = trak.is_some() && font.table_data(Tag::new(b"STAT")).is_some();
        let track_at = |pt: f32| trak.as_ref().map_or(0.0, |trak| normal_track(trak, pt));
        Some(SystemUiFace {
            id,
            family,
            axes,
            units_per_em,
            line_height,
            default_tracking: if applied {
                track_at(DEFAULT_PT).round()
            } else {
                0.0
            },
            label_tracking: track_at(LABEL_SIZE),
        })
    }

    /// Its optical size axis, if it has one.
    pub fn opsz(&self) -> Option<&Axis> {
        self.axes.iter().find(|axis| axis.tag == "opsz")
    }

    /// The optical size CoreText draws it at `pt` points with: `pt`,
    /// clamped to the axis.
    pub fn core_text_opsz(&self, pt: f32) -> Option<f32> {
        self.opsz().map(|axis| pt.clamp(axis.min, axis.max))
    }

    /// Whether cosmic-text draws it at `pt` points as Flutter drew it: the
    /// optical size's default where CoreText puts both `pt` and
    /// [`DEFAULT_PT`], and no tracking added (see the module docs).
    pub fn draws_as_flutter(&self, pt: f32) -> bool {
        let design = self.opsz().is_none_or(|axis| {
            [pt, DEFAULT_PT]
                .iter()
                .all(|size| size.clamp(axis.min, axis.max) == axis.default)
        });
        design && self.default_tracking == 0.0
    }
}

/// The `trak` table's horizontal tracking for the normal track (0) at
/// `pt` points, in font units, as HarfBuzz and harfrust compute it: a
/// track's per-size values interpolated linearly between the sizes around
/// `pt` (the first or last value outside them), and between the two
/// tracks around 0 when none is 0.
pub fn normal_track(trak: &Trak<'_>, pt: f32) -> f32 {
    let Some(Ok(data)) = trak.horiz() else {
        return 0.0;
    };
    let offsets = trak.offset_data();
    let Ok(sizes) = data.size_table(offsets) else {
        return 0.0;
    };
    let tracks = data.track_table();
    if sizes.is_empty() || tracks.is_empty() {
        return 0.0;
    }
    let value = |index: usize| -> f32 {
        let Ok(values) = tracks[index].per_size_values(offsets, data.n_sizes()) else {
            return 0.0;
        };
        let n = sizes.len().min(values.len());
        if n == 0 {
            return 0.0;
        }
        for i in 0..n {
            let size = sizes[i].get().to_f32();
            if size >= pt {
                let v1 = f32::from(values[i].get());
                if i == 0 {
                    return v1;
                }
                let s0 = sizes[i - 1].get().to_f32();
                let v0 = f32::from(values[i - 1].get());
                if (size - s0).abs() < f32::EPSILON {
                    return (v0 + v1) * 0.5;
                }
                return v0 + (pt - s0) / (size - s0) * (v1 - v0);
            }
        }
        f32::from(values[n - 1].get())
    };
    let track = |index: usize| tracks[index].track().to_f32();
    let (mut i, mut j) = (0, tracks.len() - 1);
    while i + 1 < tracks.len() && track(i + 1) <= 0.0 {
        i += 1;
    }
    while j > 0 && track(j - 1) >= 0.0 {
        j -= 1;
    }
    if i == j {
        return value(i);
    }
    let (t0, t1) = (track(i), track(j));
    let along = if (t1 - t0).abs() < f32::EPSILON {
        0.0
    } else {
        -t0 / (t1 - t0)
    };
    let (a, b) = (value(i), value(j));
    a + along * (b - a)
}

#[cfg(test)]
mod tests;
