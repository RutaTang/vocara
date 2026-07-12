import 'package:flutter/foundation.dart';

/// Hand-maintained license entries for components that ship inside the app
/// bundle but are NOT pub packages, so Flutter's automatic license collection
/// (which covers every Dart dependency) never sees them:
///
///  * the native media frameworks bundled by media_kit_libs_macos_video,
///    enumerated from the built app's Contents/Frameworks directory;
///  * the Rust crates linked into vocara_engine.framework, from
///    rust/vocara_engine/Cargo.toml.
///
/// Full license texts are not embedded; each entry states the component, its
/// license, and where the authoritative full text lives. Keep this list in
/// sync with the built app's Frameworks directory and Cargo.toml when either
/// changes.
///
/// [registerBundledComponentLicenses] is idempotent — call it before opening
/// the license page.

bool _registered = false;

/// Adds the hand-maintained entries to [LicenseRegistry] exactly once.
void registerBundledComponentLicenses() {
  if (_registered) return;
  _registered = true;
  LicenseRegistry.addLicense(
    () => Stream<LicenseEntry>.fromIterable([
      for (final c in _components) LicenseEntryWithLineBreaks([c.name], c.text),
    ]),
  );
}

class _Component {
  const _Component(this.name, this.text);

  final String name;
  final String text;
}

String _entry({
  required String component,
  required String license,
  required String fullText,
  String? ships,
  String? note,
}) {
  final b = StringBuffer()
    ..writeln(component)
    ..writeln()
    ..writeln('License: $license')
    ..writeln('Full text: $fullText');
  if (ships != null) b.writeln('Ships as: $ships');
  if (note != null) b.writeln(note);
  return b.toString().trimRight();
}

/// The bundled non-pub components. Framework names refer to the built app's
/// Contents/Frameworks directory (media_kit_libs_macos_video build of the
/// mpv/FFmpeg stack), crate versions to rust/vocara_engine/Cargo.toml.
final List<_Component> _components = [
  _Component(
    'mpv (libmpv)',
    _entry(
      component: 'mpv / libmpv — media playback engine',
      license: 'GNU Lesser General Public License v2.1 or later (LGPL-2.1+)',
      fullText: 'https://github.com/mpv-player/mpv/blob/master/LICENSE.LGPL',
      ships: 'Mpv.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'FFmpeg',
    _entry(
      component: 'FFmpeg — audio/video decoding libraries',
      license: 'GNU Lesser General Public License v3 (LGPL-3.0)',
      fullText: 'https://ffmpeg.org/legal.html',
      ships:
          'Avcodec.framework, Avformat.framework, Avfilter.framework, '
          'Avutil.framework, Swresample.framework, Swscale.framework '
          '(via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'libass',
    _entry(
      component: 'libass — subtitle rendering',
      license: 'ISC License',
      fullText: 'https://github.com/libass/libass/blob/master/COPYING',
      ships: 'Ass.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'dav1d',
    _entry(
      component: 'dav1d — AV1 decoder (VideoLAN)',
      license: 'BSD 2-Clause License',
      fullText:
          'https://code.videolan.org/videolan/dav1d/-/blob/master/COPYING',
      ships: 'Dav1d.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'FreeType',
    _entry(
      component: 'FreeType — font rendering',
      license: 'FreeType License (FTL, BSD-style with attribution)',
      fullText:
          'https://gitlab.freedesktop.org/freetype/freetype/-/blob/master/docs/FTL.TXT',
      ships: 'Freetype.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'FriBidi',
    _entry(
      component: 'GNU FriBidi — bidirectional text',
      license: 'GNU Lesser General Public License v2.1 or later (LGPL-2.1+)',
      fullText: 'https://github.com/fribidi/fribidi/blob/master/COPYING',
      ships: 'Fribidi.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'HarfBuzz',
    _entry(
      component: 'HarfBuzz — text shaping',
      license: 'MIT ("Old MIT") License',
      fullText: 'https://github.com/harfbuzz/harfbuzz/blob/main/COPYING',
      ships: 'Harfbuzz.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'Mbed TLS',
    _entry(
      component: 'Mbed TLS — TLS/crypto',
      license: 'Apache License 2.0',
      fullText: 'https://github.com/Mbed-TLS/mbedtls/blob/development/LICENSE',
      ships:
          'Mbedtls.framework, Mbedcrypto.framework, Mbedx509.framework '
          '(via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'libpng',
    _entry(
      component: 'libpng — PNG image decoding',
      license: 'PNG Reference Library License v2',
      fullText: 'http://www.libpng.org/pub/png/src/libpng-LICENSE.txt',
      ships: 'Png16.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'uchardet',
    _entry(
      component: 'uchardet — character-set detection',
      license: 'Mozilla Public License 1.1 (tri-licensed MPL 1.1 / GPL / LGPL)',
      fullText:
          'https://gitlab.freedesktop.org/uchardet/uchardet/-/blob/master/COPYING',
      ships: 'Uchardet.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'libxml2',
    _entry(
      component: 'libxml2 — XML parsing',
      license: 'MIT License',
      fullText:
          'https://gitlab.gnome.org/GNOME/libxml2/-/blob/master/Copyright',
      ships: 'Xml2.framework (via media_kit_libs_macos_video)',
    ),
  ),
  _Component(
    'SQLite',
    _entry(
      component: 'SQLite — the library database engine',
      license: 'Public domain',
      fullText: 'https://sqlite.org/copyright.html',
      ships: 'sqlite3.framework (via sqlite3_flutter_libs)',
    ),
  ),
  // Rust crates statically linked into vocara_engine.framework. The engine's
  // own code is first-party (covered by the app copyright); these are its
  // direct dependencies from rust/vocara_engine/Cargo.toml.
  _Component(
    'cpal (Rust crate)',
    _entry(
      component: 'cpal 0.18 — cross-platform audio I/O',
      license: 'Apache License 2.0',
      fullText: 'https://github.com/RustAudio/cpal/blob/master/LICENSE',
      ships: 'statically linked into vocara_engine.framework',
    ),
  ),
  _Component(
    'Symphonia (Rust crate)',
    _entry(
      component: 'symphonia 0.5 — audio decoding',
      license: 'Mozilla Public License 2.0 (MPL-2.0)',
      fullText: 'https://github.com/pdeljanov/Symphonia/blob/master/LICENSE',
      ships: 'statically linked into vocara_engine.framework',
    ),
  ),
  _Component(
    'objc2 Apple framework crates (Rust)',
    _entry(
      component:
          'objc2-audio-toolbox, objc2-core-audio, objc2-core-audio-types, '
          'objc2-core-foundation 0.3 — Apple framework bindings',
      license: 'Zlib OR Apache-2.0 OR MIT (at your option)',
      fullText: 'https://github.com/madsmtm/objc2/blob/master/LICENSE.md',
      ships: 'statically linked into vocara_engine.framework',
      note:
          'Transitive Rust dependencies are recorded in '
          'rust/vocara_engine/Cargo.lock.',
    ),
  ),
];
