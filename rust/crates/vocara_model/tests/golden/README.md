# Golden files from the Flutter client

Each `<name>.txt` here records what the Flutter app's own Dart code computed
for a set of inputs (JSON encoding, the models' JSON round trips, lyrics
parsing, the diagnostics scrubber, the voice metrics). The `*_golden.rs` tests
check that the Rust port reproduces them byte for byte, so data written by
either app reads the same in the other.

`<name>.gen.dart` is the Dart program that wrote `<name>.txt`, kept as the
record of how each golden was made: its header says how it was run.

The Flutter client (`client/`) has been retired, so these generators no longer
run from this tree. The goldens are now the specification. To regenerate one,
check out the last commit that still has `client/`
(`git log --diff-filter=D -1 -- client/pubspec.yaml`, then its parent) and run
the generator there with the Dart SDK.
