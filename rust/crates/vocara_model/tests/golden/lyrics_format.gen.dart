// Writes lyrics_format.txt: what the Flutter app's lyrics_format.dart makes
// of tricky lyrics texts, for the Rust port's hand-written matchers to
// reproduce exactly. One JSON object a line:
// {"input": text, "looksLikeLrc", "isLrcDocument", "plain": [[text, timeMs]],
//  "lrc": [[text, timeMs]] or "THROWS", "toLrc": text of the parsed lines}.
// Run like models_json.gen.dart.
import 'dart:convert';

import 'package:vocara/utils/lyrics_format.dart';

const inputs = [
  '',
  '\n',
  '\r\n\r\n',
  'plain',
  '﻿bom line\n  indented  \n\t\n',
  '[00:01.00]one\r[00:02.00]two\r\n[00:03.00]three',
  '[ti:Song]\n[ar:Artist]\n[00:12.50]hello\n[01:03.250]world\nuntimed',
  '[00:30.00]chorus\n[00:05.00]intro\n[00:10.00][00:20.00]ah',
  '[00:01.5]a\n[00:01.05]b\n[00:01.005]c\n[00:01:50]d\n[00:01.]e\n[00:01.1234]f',
  '[0:00]a\n[000:00]b\n[1000:00]c\n[00:000]d\n[00:5]e\n[100:59.999]f',
  '[offset:+500]\n[00:10.00]a\n[00:00.20]b',
  '[OFFSET: -250 ]\n[00:10.00]a',
  '[offset:+99999999999999999999999]\n[00:10.00]a',
  '[offset:-9223372036854775807]\n[00:01.00]a',
  '[offset:500]\n[offset:100]\n[00:01.00]a',
  '[offset:1.5]\n[00:01.00]a',
  'I waited [04:33] for you\nand you came',
  '[00:12.50]hello\nlyrics by someone',
  '[ti:Song]\n[ar:Artist]',
  '[#:comment]\n[00:01.00]x',
  '[1x:y]\n[00:01.00]x',
  '[ti:a]b]\n[00:01.00]x',
  '[ti:]\n[00:01.00]x',
  '[ti]\n[00:01.00]x',
  '[t:i:t]\n[00:01.00]x',
  '  [00:01.00]  spaced text  [00:02.00] tail  ',
  '[00:01.00]',
  '[00:01.00][00:01.00]same\n[00:01.00]other',
  '[00:01.00]café 中文 \u{1F600}',
  '[[00:01.00]]bracketed',
  'text [00:05.00] inside',
  '[00:01.00]a b',
  '[00:01.00]a\u0085b\n',
  '[١٢:00]arabic digits',
];

List<List<Object?>> lines(List<dynamic> ls) => [
  for (final l in ls) [l.text, l.timeMs],
];

void main() {
  for (final input in inputs) {
    Object lrc;
    String? round;
    try {
      final parsed = parseLrc(input);
      lrc = lines(parsed);
      round = toLrc(parsed);
    } on FormatException {
      lrc = 'THROWS';
    }
    print(
      jsonEncode({
        'input': input,
        'looksLikeLrc': looksLikeLrc(input),
        'isLrcDocument': isLrcDocument(input),
        'plain': lines(parsePlainLyrics(input)),
        'plainText': lyricsToPlainText(parsePlainLyrics(input)),
        'lrc': lrc,
        'toLrc': round,
      }),
    );
  }
  final crafted = '${'[0:00]' * 40000}${'x' * 100000}';
  String verdict;
  try {
    parseLrc(crafted);
    verdict = 'parsed';
  } on FormatException {
    verdict = 'THROWS';
  }
  print(jsonEncode({'crafted': verdict}));
  // Alignments of small edits, both directions.
  const texts = <List<String>>[
    ['a', 'b', 'c', 'b', 'a'],
    ['a', 'c', 'b', 'a'],
    ['x', 'a', 'b', 'c', 'b', 'a', 'y'],
    ['', 'a', '', 'a', ''],
    ['b', 'a'],
    [],
  ];
  for (final prev in texts) {
    for (final next in texts) {
      print(jsonEncode({'prev': prev, 'next': next, 'align': alignLines(prev, next)}));
    }
  }
}
