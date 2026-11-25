// SPDX-License-Identifier: MIT OR Apache-2.0

use harfrust::Shaper;
use linebender_resource_handle::{Blob, FontData};
use skrifa::raw::{ReadError, TableProvider as _};
use skrifa::{metrics::Metrics, prelude::*};
// re-export skrifa
pub use skrifa;
// re-export peniko::Font;
#[cfg(feature = "peniko")]
pub use linebender_resource_handle::FontData as PenikoFont;

use core::fmt;

use alloc::sync::Arc;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use self_cell::self_cell;

pub mod fallback;
pub use fallback::{Fallback, PlatformFallback};

pub use self::system::*;
mod system;

struct OwnedFaceData {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    // vocara: the face's table directory without its `trak` table, which
    // the shaper reads in its place (`FaceInstance::untracked`).
    untracked_directory: Option<Vec<u8>>,
    shaper_data: harfrust::ShaperData,
    shaper_instance: harfrust::ShaperInstance,
    metrics: Metrics,
}

/// vocara: a face drawn at a fixed instance of its variation axes, set on
/// a [`FontSystem`] for one face ([`FontSystem::set_face_instance`]).
/// Every use of the face takes it: its metrics, the shaper's advances,
/// positioning and substitutions, and the outlines the rasterizer draws.
///
/// Upstream varies a face along its weight axis alone (the weight asked
/// for), every other axis at its default.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInstance {
    /// Its coordinates in user units, by axis tag: these axes are set,
    /// after the weight asked for (so a `wght` here overrides it), each
    /// value clamped to its axis; the others stay at their defaults.
    pub coords: Vec<([u8; 4], f32)>,
    /// Whether the shaper leaves the face's `trak` table unapplied (harfrust
    /// applies it to a face with a `STAT` table, at 12 pt).
    pub untracked: bool,
}

/// vocara: the table directory of `font` without its `trak` record, if it
/// has one.
fn untracked_directory(font: &FontRef<'_>) -> Option<Vec<u8>> {
    let directory = &font.table_directory;
    let records = directory.table_records();
    let trak = Tag::new(b"trak");
    if !records.iter().any(|record| record.tag() == trak) {
        return None;
    }
    let kept: Vec<_> = records.iter().filter(|r| r.tag() != trak).collect();
    let mut bytes = Vec::with_capacity(12 + 16 * kept.len());
    bytes.extend_from_slice(&directory.sfnt_version().to_be_bytes());
    bytes.extend_from_slice(&(kept.len() as u16).to_be_bytes());
    bytes.extend_from_slice(&directory.search_range().to_be_bytes());
    bytes.extend_from_slice(&directory.entry_selector().to_be_bytes());
    bytes.extend_from_slice(&directory.range_shift().to_be_bytes());
    for record in kept {
        bytes.extend_from_slice(&record.tag().to_be_bytes());
        bytes.extend_from_slice(&record.checksum().to_be_bytes());
        bytes.extend_from_slice(&record.offset().to_be_bytes());
        bytes.extend_from_slice(&record.length().to_be_bytes());
    }
    Some(bytes)
}

/// vocara: `font` as the shaper reads it: with `directory` (its own
/// without `trak`) in place of its table directory, if any.
fn shaped_font<'a>(
    font: FontRef<'a>,
    directory: Option<&'a [u8]>,
) -> Result<FontRef<'a>, ReadError> {
    use skrifa::raw::{FontData, FontRead, TableDirectory};
    let mut font = font;
    if let Some(directory) = directory {
        font.table_directory = TableDirectory::read(FontData::new(directory))?;
    }
    Ok(font)
}

self_cell!(
    struct OwnedFace {
        owner: OwnedFaceData,

        #[covariant]
        dependent: Shaper,
    }
);

struct FontMonospaceFallback {
    monospace_em_width: Option<f32>,
    scripts: Vec<[u8; 4]>,
    unicode_codepoints: Vec<u32>,
}

/// A font
pub struct Font {
    #[cfg(feature = "swash")]
    swash: (u32, swash::CacheKey),
    harfrust: OwnedFace,
    data: FontData,
    id: fontdb::ID,
    monospace_fallback: Option<FontMonospaceFallback>,
    // vocara: the normalized coordinates of its `FaceInstance`, if it has
    // one, for the rasterizer.
    instance_coords: Option<Vec<i16>>,
    // vocara: its line gap, in its units ([`Font::line_gap`]).
    line_gap: f32,
}

impl fmt::Debug for Font {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Font {
    pub const fn id(&self) -> fontdb::ID {
        self.id
    }

    pub fn monospace_em_width(&self) -> Option<f32> {
        self.monospace_fallback
            .as_ref()
            .and_then(|x| x.monospace_em_width)
    }

    pub fn scripts(&self) -> &[[u8; 4]] {
        self.monospace_fallback.as_ref().map_or(&[], |x| &x.scripts)
    }

    pub fn unicode_codepoints(&self) -> &[u32] {
        self.monospace_fallback
            .as_ref()
            .map_or(&[], |x| &x.unicode_codepoints)
    }

    pub fn data(&self) -> &[u8] {
        self.data.data.data()
    }

    pub fn shaper(&self) -> &harfrust::Shaper<'_> {
        self.harfrust.borrow_dependent()
    }

    pub(crate) fn shaper_instance(&self) -> &harfrust::ShaperInstance {
        &self.harfrust.borrow_owner().shaper_instance
    }

    pub fn metrics(&self) -> &Metrics {
        &self.harfrust.borrow_owner().metrics
    }

    /// vocara: the normalized coordinates (F2Dot14) of the face's
    /// [`FaceInstance`], if it has one.
    pub fn instance_coords(&self) -> Option<&[i16]> {
        self.instance_coords.as_deref()
    }

    /// vocara: its line gap, in its units, as CoreText gives it
    /// (`CTFontGetLeading`): `hhea`'s `lineGap` with its `MVAR` delta
    /// (`hlgp`) at its instance, whether or not its `OS/2` table asks for
    /// its typographic metrics (which [`Font::metrics`] then takes: Lucide's
    /// line gap there is 90 units, `hhea`'s 0).
    pub fn line_gap(&self) -> f32 {
        self.line_gap
    }

    #[cfg(feature = "peniko")]
    pub fn as_peniko(&self) -> PenikoFont {
        self.data.clone()
    }

    #[cfg(feature = "swash")]
    pub fn as_swash(&self) -> swash::FontRef<'_> {
        let swash = &self.swash;
        swash::FontRef {
            data: self.data(),
            offset: swash.0,
            key: swash.1,
        }
    }
}

impl Font {
    pub fn new(db: &fontdb::Database, id: fontdb::ID, weight: fontdb::Weight) -> Option<Self> {
        Self::new_with_instance(db, id, weight, None)
    }

    /// vocara: [`Font::new`], drawn at `instance` if given.
    pub fn new_with_instance(
        db: &fontdb::Database,
        id: fontdb::ID,
        weight: fontdb::Weight,
        instance: Option<&FaceInstance>,
    ) -> Option<Self> {
        let info = db.face(id)?;

        let data = match &info.source {
            fontdb::Source::Binary(data) => Arc::clone(data),
            #[cfg(feature = "std")]
            fontdb::Source::File(path) => {
                log::warn!("Unsupported fontdb Source::File('{}')", path.display());
                return None;
            }
            #[cfg(feature = "std")]
            fontdb::Source::SharedFile(_path, data) => Arc::clone(data),
        };

        // It's a bit unfortunate but we need to parse the data into a `FontRef`
        // twice--once to construct the HarfRust `ShaperInstance` and
        // `ShaperData`, and once to create the persistent `FontRef` tied to the
        // lifetime of the face data.
        let font_ref = FontRef::from_index((*data).as_ref(), info.index).ok()?;
        // vocara: the instance's coordinates after the weight's.
        let instance_settings = instance
            .into_iter()
            .flat_map(|instance| instance.coords.iter())
            .map(|(tag, value)| (Tag::new(tag), *value));
        let location = font_ref.axes().location(
            core::iter::once((Tag::new(b"wght"), weight.0 as f32)).chain(instance_settings),
        );
        let instance_coords =
            instance.map(|_| location.coords().iter().map(|c| c.to_bits()).collect());
        let untracked_directory = instance
            .filter(|instance| instance.untracked)
            .and_then(|_| untracked_directory(&font_ref));
        let metrics = font_ref.metrics(Size::unscaled(), &location);
        // vocara: `hhea`'s line gap there, as CoreText takes it.
        let line_gap = font_ref.hhea().map_or(0.0, |hhea| {
            let delta = font_ref
                .mvar()
                .ok()
                .filter(|_| !location.coords().is_empty())
                .and_then(|mvar| mvar.metric_delta(Tag::new(b"hlgp"), location.coords()).ok())
                .map_or(0.0, |delta| delta.to_f64() as f32);
            f32::from(hhea.line_gap().to_i16()) + delta
        });

        let monospace_fallback = if cfg!(feature = "monospace_fallback") {
            (|| {
                let glyph_metrics = font_ref.glyph_metrics(Size::unscaled(), &location);
                let charmap = font_ref.charmap();
                let monospace_em_width = info
                    .monospaced
                    .then(|| {
                        let hor_advance = glyph_metrics.advance_width(charmap.map(' ')?)?;
                        let upem = metrics.units_per_em;
                        Some(hor_advance / f32::from(upem))
                    })
                    .flatten();

                if info.monospaced && monospace_em_width.is_none() {
                    None?;
                }

                let scripts = font_ref
                    .gpos()
                    .ok()?
                    .script_list()
                    .ok()?
                    .script_records()
                    .iter()
                    .chain(
                        font_ref
                            .gsub()
                            .ok()?
                            .script_list()
                            .ok()?
                            .script_records()
                            .iter(),
                    )
                    .map(|script| script.script_tag().into_bytes())
                    .collect();

                let mut unicode_codepoints = Vec::new();

                for (code_point, _) in charmap.mappings() {
                    unicode_codepoints.push(code_point);
                }

                unicode_codepoints.shrink_to_fit();

                Some(FontMonospaceFallback {
                    monospace_em_width,
                    scripts,
                    unicode_codepoints,
                })
            })()
        } else {
            None
        };

        let (shaper_instance, shaper_data) = {
            (
                harfrust::ShaperInstance::from_coords(&font_ref, location.coords().iter().copied()),
                // vocara: as the shaper reads the face.
                harfrust::ShaperData::new(
                    &shaped_font(font_ref.clone(), untracked_directory.as_deref()).ok()?,
                ),
            )
        };

        Some(Self {
            id: info.id,
            monospace_fallback,
            #[cfg(feature = "swash")]
            swash: {
                let swash = swash::FontRef::from_index((*data).as_ref(), info.index as usize)?;
                (swash.offset, swash.key)
            },
            harfrust: OwnedFace::try_new(
                OwnedFaceData {
                    data: Arc::clone(&data),
                    untracked_directory,
                    shaper_data,
                    shaper_instance,
                    metrics,
                },
                |OwnedFaceData {
                     data,
                     untracked_directory,
                     shaper_data,
                     shaper_instance,
                     ..
                 }| {
                    // vocara: as the shaper reads the face.
                    let font_ref = shaped_font(
                        FontRef::from_index((**data).as_ref(), info.index)?,
                        untracked_directory.as_deref(),
                    )?;
                    let shaper = shaper_data
                        .shaper(&font_ref)
                        .instance(Some(shaper_instance))
                        .build();
                    Ok::<_, ReadError>(shaper)
                },
            )
            .ok()?,
            data: FontData::new(Blob::new(data), info.index),
            instance_coords,
            line_gap,
        })
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_fonts_load_time() {
        use crate::FontSystem;
        use sys_locale::get_locale;

        #[cfg(not(target_arch = "wasm32"))]
        let now = std::time::Instant::now();

        let mut db = fontdb::Database::new();
        let locale = get_locale().expect("Local available");
        db.load_system_fonts();
        FontSystem::new_with_locale_and_db(locale, db);

        #[cfg(not(target_arch = "wasm32"))]
        println!("Fonts load time {}ms.", now.elapsed().as_millis());
    }
}
