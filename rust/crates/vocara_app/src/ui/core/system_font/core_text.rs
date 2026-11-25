//! CoreText, CoreFoundation and AppKit, as much as the macOS tests ask
//! them: the fonts Flutter's Skia made for a label, step for step.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

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
    fn CFDictionaryGetKeysAndValues(dict: CFTypeRef, keys: *mut CFTypeRef, values: *mut CFTypeRef);
    fn CFStringCreateWithCString(
        allocator: CFTypeRef,
        s: *const c_char,
        encoding: u32,
    ) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFNumberIsFloatType(number: CFTypeRef) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
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
    static kCTFontNameAttribute: CFTypeRef;
    static kCTFontStyleNameAttribute: CFTypeRef;
    static kCTFontTraitsAttribute: CFTypeRef;
    static kCTFontVariationAttribute: CFTypeRef;
    static kCTFontWeightTrait: CFTypeRef;
    static kCTFontWidthTrait: CFTypeRef;
    static kCTFontSlantTrait: CFTypeRef;
    static kCTFontURLAttribute: CFTypeRef;
    fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: CFTypeRef) -> CFTypeRef;
    fn CTFontCopyFamilyName(font: CFTypeRef) -> CFTypeRef;
    fn CTFontCopyPostScriptName(font: CFTypeRef) -> CFTypeRef;
    fn CTFontCopyFontDescriptor(font: CFTypeRef) -> CFTypeRef;
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

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSFontWeightUltraLight: f64;
    static NSFontWeightThin: f64;
    static NSFontWeightLight: f64;
    static NSFontWeightRegular: f64;
    static NSFontWeightMedium: f64;
    static NSFontWeightSemibold: f64;
    static NSFontWeightBold: f64;
    static NSFontWeightHeavy: f64;
    static NSFontWeightBlack: f64;
}

fn string(s: &str) -> CFTypeRef {
    let s = CString::new(s).expect("no NUL");
    // SAFETY: a valid C string; the result is a CFString (leaked).
    unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) }
}

fn to_string(s: CFTypeRef) -> Option<String> {
    // SAFETY: only a CFString is read.
    if s.is_null() || unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
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

/// A CFNumber as Skia's `find_dict_CGFloat` reads it: only a float type.
fn cg_float(value: CFTypeRef) -> Option<f64> {
    // SAFETY: only a CFNumber is asked whether it is a float.
    let float = !value.is_null()
        && unsafe { CFGetTypeID(value) == CFNumberGetTypeID() && CFNumberIsFloatType(value) != 0 };
    if float {
        number(value)
    } else {
        None
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

/// A variation dictionary (axis identifier to value) by axis tag.
fn variation_of(variation: CFTypeRef) -> Vec<(u32, f64)> {
    // SAFETY: only a CFDictionary is read, its keys and values as numbers.
    unsafe {
        if variation.is_null() || CFGetTypeID(variation) != CFDictionaryGetTypeID() {
            return Vec::new();
        }
        let count = CFDictionaryGetCount(variation).max(0) as usize;
        let mut keys = vec![std::ptr::null(); count];
        let mut values = vec![std::ptr::null(); count];
        CFDictionaryGetKeysAndValues(variation, keys.as_mut_ptr(), values.as_mut_ptr());
        let mut out: Vec<(u32, f64)> = keys
            .iter()
            .zip(&values)
            .filter_map(|(k, v)| Some((integer(*k)? as u32, number(*v)?)))
            .collect();
        out.sort_by_key(|(tag, _)| *tag);
        out
    }
}

/// A tag as its four letters.
pub fn tag_name(tag: u32) -> String {
    String::from_utf8_lossy(&tag.to_be_bytes()).into_owned()
}

/// AppKit's `NSFontWeight` constants with -1 and 1 at either end, as
/// Skia reads them (`SkCTFontGetNSFontWeightMapping`).
pub fn ns_font_weights() -> [f64; 11] {
    // SAFETY: AppKit's exported constants.
    unsafe {
        [
            -1.0,
            NSFontWeightUltraLight,
            NSFontWeightThin,
            NSFontWeightLight,
            NSFontWeightRegular,
            NSFontWeightMedium,
            NSFontWeightSemibold,
            NSFontWeightBold,
            NSFontWeightHeavy,
            NSFontWeightBlack,
            1.0,
        ]
    }
}

/// Skia's CSS weight of a CoreText weight trait through `weights`
/// (`ct_weight_to_fontstyle`: linear between them, rounded half up).
pub fn skia_weight(weights: &[f64; 11], ct: f64) -> i32 {
    if ct < weights[0] {
        return 0;
    }
    for (i, pair) in weights.windows(2).enumerate() {
        if ct < pair[1] {
            let (from, to) = (100.0 * i as f64, 100.0 * (i + 1) as f64);
            let css = from + ((ct - pair[0]) * (to - from)) / (pair[1] - pair[0]);
            return (css + 0.5) as i32;
        }
    }
    1000
}

/// Skia's CSS width of a CoreText width trait (`ct_width_to_fontstyle`:
/// -0.5 to 0.5 at 0 to 10, rounded half up).
pub fn skia_width(ct: f64) -> i32 {
    if ct < -0.5 {
        return 0;
    }
    if ct < 0.5 {
        return (10.0 * (ct + 0.5) + 0.5) as i32;
    }
    10
}

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

/// One of the descriptors CoreText matches to a family, as Skia sees it.
#[derive(Clone)]
pub struct Descriptor {
    raw: CFTypeRef,
    pub name: String,
    pub style_name: String,
    /// Its weight, width and slant traits (0 where it has none, as
    /// `SkCTFontDescriptorGetSkFontStyle` takes a missing one).
    pub traits: (f64, f64, f64),
    /// Skia's style of it: CSS weight, width (0 to 10), italic.
    pub style: (i32, i32, bool),
    /// Its variation attribute, by axis tag.
    pub variation: Vec<(u32, f64)>,
}

impl std::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let variation: Vec<String> = self
            .variation
            .iter()
            .map(|(tag, value)| format!("{} {value}", tag_name(*tag)))
            .collect();
        write!(
            f,
            "{} ({}): traits {:?}, Skia's style {:?}, variation [{}]",
            self.name,
            self.style_name,
            self.traits,
            self.style,
            variation.join(", ")
        )
    }
}

/// The traits of a descriptor's (or font's) traits dictionary.
fn traits_of(traits: CFTypeRef) -> (f64, f64, f64) {
    // SAFETY: only a CFDictionary is read, its values as CGFloats.
    unsafe {
        if traits.is_null() || CFGetTypeID(traits) != CFDictionaryGetTypeID() {
            return (0.0, 0.0, 0.0);
        }
        let get = |key| cg_float(CFDictionaryGetValue(traits, key)).unwrap_or(0.0);
        (
            get(kCTFontWeightTrait),
            get(kCTFontWidthTrait),
            get(kCTFontSlantTrait),
        )
    }
}

/// The descriptors CoreText matches to `family`, in its order, as Skia's
/// `SkFontStyleSet_Mac` asks for them (the family name mandatory).
pub fn family_descriptors(family: &str) -> Vec<Descriptor> {
    let weights = ns_font_weights();
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
        (0..CFArrayGetCount(matches))
            .map(|i| {
                let raw = CFArrayGetValueAtIndex(matches, i);
                let traits = traits_of(CTFontDescriptorCopyAttribute(raw, kCTFontTraitsAttribute));
                Descriptor {
                    raw,
                    name: to_string(CTFontDescriptorCopyAttribute(raw, kCTFontNameAttribute))
                        .unwrap_or_default(),
                    style_name: to_string(CTFontDescriptorCopyAttribute(
                        raw,
                        kCTFontStyleNameAttribute,
                    ))
                    .unwrap_or_default(),
                    traits,
                    style: (
                        skia_weight(&weights, traits.0),
                        skia_width(traits.1),
                        traits.2 != 0.0,
                    ),
                    variation: variation_of(CTFontDescriptorCopyAttribute(
                        raw,
                        kCTFontVariationAttribute,
                    )),
                }
            })
            .collect()
    }
}

/// `findMatchingDesc` for a style of `weight`, a normal width, upright:
/// the first descriptor of the least `compute_metric`.
pub fn skia_match(descriptors: &[Descriptor], weight: i32) -> &Descriptor {
    let metric = |d: &Descriptor| {
        let (w, width, italic) = d.style;
        (weight - w).pow(2) + ((5 - width) * 100).pow(2) + (i32::from(italic) * 900).pow(2)
    };
    let mut best: Option<(&Descriptor, i32)> = None;
    for d in descriptors {
        let m = metric(d);
        if m == 0 {
            return d;
        }
        if best.is_none_or(|(_, b)| m < b) {
            best = Some((d, m));
        }
    }
    best.expect("a descriptor of the family").0
}

/// A CoreText font (leaked: the test's).
#[derive(Clone, Copy)]
pub struct CtFont(CFTypeRef);

impl Descriptor {
    /// `create_from_desc`: the typeface Skia made of it, at size 0.
    pub fn skia_typeface(&self) -> CtFont {
        // SAFETY: a CTFontDescriptor.
        let font = unsafe { CTFontCreateWithFontDescriptor(self.raw, 0.0, std::ptr::null()) };
        assert!(!font.is_null(), "a font of {}", self.name);
        CtFont(font)
    }
}

impl CtFont {
    pub fn size(self) -> f64 {
        // SAFETY: a CTFont.
        unsafe { CTFontGetSize(self.0) }
    }

    pub fn postscript_name(self) -> String {
        // SAFETY: a CTFont.
        to_string(unsafe { CTFontCopyPostScriptName(self.0) }).unwrap_or_default()
    }

    /// Skia's style weight of the typeface (`SkTypeface_Mac::Make`: its
    /// font descriptor's traits).
    pub fn skia_weight(self) -> i32 {
        // SAFETY: a CTFont and its descriptor.
        let traits = unsafe {
            CTFontDescriptorCopyAttribute(CTFontCopyFontDescriptor(self.0), kCTFontTraitsAttribute)
        };
        skia_weight(&ns_font_weights(), traits_of(traits).0)
    }

    /// Its `NSCTFontOpticalSizeAttribute`, if a number.
    pub fn optical_size_attribute(self) -> Option<f64> {
        // SAFETY: a CTFont; the attribute is only read as a number.
        unsafe {
            let value = CTFontCopyAttribute(self.0, string("NSCTFontOpticalSizeAttribute"));
            if let Some(s) = to_string(value) {
                println!("NSCTFontOpticalSizeAttribute: {s:?}");
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
        // SAFETY: CoreFoundation and CoreText calls on what they return.
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
            let copy = CTFontCreateCopyWithAttributes(self.0, size, std::ptr::null(), descriptor);
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
        // SAFETY: a CTFont.
        variation_of(unsafe { CTFontCopyVariation(self.0) })
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
