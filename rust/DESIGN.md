# Vocara design system

The look the app is drawn in. It refines the Studio look the port started
from (the Flutter app's): the same layouts, panels, features and colour
identity (charcoal and coral), drawn to the standard of a professional
desktop audio tool. Where this file and a Flutter-parity comment in the code
disagree, this file wins: the visual details below deliberately depart from
the Flutter app (`PORTING.md`, Design refresh). Behaviour, data and layout
structure do not change.

The approved mockups (the "Polish: before & after" page of the design
canvas) are the reference for every screen they show: the practice
workspace, the workspace with the mixer dock, and the library. Their sources
are `.dc.html` files (HTML with inline styles); every value below comes from
them.

## Principles

1. **One accent, used for meaning.** Coral marks the selection, record and
   an enabled effect. Everything else (sliders, faders, toggles, labels,
   lyrics) is neutral. Status colours (in tune, flat, meters) stay, muted.
2. **Desktop controls, not phone controls.** Small, precise controls: 2 px
   slider tracks, 10 px knobs, segmented controls, power buttons. No
   Material switches, no chips, no pill buttons, no outlined coral buttons.
3. **Sentence case.** No uppercase letter-spaced labels anywhere
   ("Inspector", "Vocal takes", "Summary", "Equalizer"). Track and clip
   names are shown as the user typed them.
4. **One scale for everything:** type, spacing, radii, control heights and
   icons (below). No in-between sizes.
5. **Numbers are tabular** (`font_features: tnum`, or a monospaced face for
   the time display) so readouts do not jitter.
6. **Data looks precise.** Waveforms are filled, symmetric peak envelopes;
   pitch is a thin line over faint target notes; meters are thin bars.

## Colour tokens

Dark only. Names are the `Studio` constants; a new name is new.

| Token | Hex | Use |
|---|---|---|
| `BACKGROUND` | `#111114` | Window, arrange, page background |
| `TOOLBAR` (new) | `#18181C` | Unified toolbar |
| `PANEL` | `#17171B` | Inspector, right panel, dock, track headers |
| `PANEL_SELECTED` (new) | `#1D1D22` | Selected track header |
| `SURFACE` | `#1B1B20` | Cards, mixer strips |
| `SURFACE_RAISED` | `#232329` | Pop-up buttons, raised controls, segmented background is `#1C1C21` |
| `CONTROL` (new) | `#26262D` | M / S / R buttons, inactive power button |
| `CONTROL_ACTIVE` (new) | `#2A2A31` | Selected tab, selected segment (`#34343D` in a segmented control) |
| `INSET` | `#0C0C0F` | Time display, meters, value fields; `#0F0F12` for graph/lane wells |
| `HOVER` | `#222228` | Row and button hover |
| `GRID` | `#18181D` | Arrange bar lines, semitone lines |
| `GRID_STRONG` | `#34343D` | Ruler major ticks, dividers inside groups |
| `BORDER` | `#26262D` | Panel and toolbar hairlines (1 px) |
| `BORDER_SUBTLE` (new) | `#1E1E24` | Row separators inside the arrange |
| `BORDER_STRONG` | `#2E2E36` | Control outlines (inset 1 px shadow) |
| `TEXT_PRIMARY` | `#ECECF1` | Names, values, body |
| `TEXT_SECONDARY` | `#A7A7B3` | Labels, inactive tabs, icons |
| `TEXT_FAINT` | `#80808D` | Metadata, ruler labels, units (≥ 4.5:1 on `BACKGROUND`) |
| `TEXT_DISABLED` (new) | `#6E6E7A` | Axis labels, disabled text (≥ 10 px only) |
| `ACCENT` | `#E07A6A` | Selection, primary emphasis |
| `ACCENT_BRIGHT` | `#EC9182` | Accent text and icons on dark (power on, current lyric time) |
| `RECORD` | `#E5574B` | Record dot, armed and muted states (`rgba(229,87,75,.20)` fill, `#F08A80` text) |
| `PLAYHEAD` | `#ECECF1` | Playhead line and ruler marker |
| `IN_TUNE` / `GOOD` | `#7CC4A6` | Sung pitch line, meters' low range `#4FAE82` |
| `WARN` (new) | `#D9B54A` | Meter upper range, solo on (`#E3C46A` text) |

The `Zinc` palette (forui's) is retired for app chrome: dialogs, menus,
settings and the library use the tokens above. Keep `Zinc` only where a
test or a third-party look needs it, or remove it.

**Track colours.** One muted colour per track, of equal lightness, used
for the 3 px header stripe, the clip and its waveform:
`#D98C7E` (coral), `#C9A46E` (ochre), `#6FB39A` (sage), `#8FA3D9`
(periwinkle), `#B48FD0` (lilac), `#8C8C99` (backing, neutral), `#B7B7C2`
(master). A clip is filled with the colour at 10 % (16 % selected), outlined
1 px at 32 % (selected: `#E8A99D`, 1 px), with a 13 px header band at 22 %
(34 % selected) holding the name at 10 px; its waveform is the colour at
92 % (45 % when muted; the clip at 60 % opacity).

## Type

Inter everywhere (SF Pro where the code already sets the system font).
Line height 16 px for 11–12 px text unless noted.

| Role | Size / weight | Colour |
|---|---|---|
| Axis, ruler, small units | 10 px regular, tabular | `TEXT_FAINT` / `TEXT_DISABLED` |
| Label, meta, value, tab in a dock, control text | 11 px regular (labels) or medium (values) | `TEXT_SECONDARY` / `TEXT_PRIMARY` |
| Section heading inside a panel | 11 px semibold | `TEXT_SECONDARY` |
| Body, track and effect names, panel titles, toolbar buttons | 12 px medium (names) or semibold (panel titles) | `TEXT_PRIMARY` |
| Lyrics, list rows, page titles in a toolbar, card titles | 13 px (lyrics 20 px line) / 13 px semibold | `TEXT_PRIMARY` |
| Page heading ("Practices") | 15 px semibold | `TEXT_PRIMARY` |
| Big readout (pitch %, metric value) | 18 px semibold, tabular | `TEXT_PRIMARY` |
| Time display | 14 px monospaced medium (JetBrains Mono-like; use the bundled mono or SF Mono), tabular | `TEXT_PRIMARY`, total `TEXT_FAINT` |

## Geometry

- Spacing on a 4 px grid: 2, 4, 6, 8, 10, 12, 14, 16, 24, 28, 32.
- Radii: 4 (M/S buttons, clips, small controls), 5–6 (pop-up buttons,
  segmented, tabs, wells), 7 (toolbar groups), 8 (cards, panels' rounded
  boxes). Round only for knobs, dots and the power button.
- Control heights: 18–20 (M/S, tab buttons in a strip), 22 (pop-up buttons,
  segmented controls, ghost buttons), 24 (panel icon buttons, panel tabs),
  28 (toolbar icon buttons), 30 (toolbar groups, time display).
- Hairlines are 1 px. Outlines on controls are an inset 1 px shadow in
  `BORDER_STRONG`, not a border that changes the size.
- Panel widths: inspector 264, track header column 212, right panel 292.

## Icons

- Line icons, round caps and joins, drawn in a 24-unit grid and shown in a
  16 px box (15 px in dense toolbars, 12–14 px inside 20–24 px buttons).
- **Optical size is equal across a row**: every glyph's visible extent
  fills the same area (about 17 of 24 units, so about 11 px at 16 px), and
  every stroke is the same width on screen (about 1.2 px). Scale a glyph
  that fills the whole grid (the settings gear) down to 75 % and thicken its
  stroke by the same factor; lengthen sparse glyphs (plus, minus). Check a
  row of icons side by side.
- Icon-only buttons have a tooltip and an accessibility label.
- Icons are `TEXT_SECONDARY`; hover `TEXT_PRIMARY`; active/pressed
  `TEXT_PRIMARY` (or `ACCENT_BRIGHT` where it marks an enabled effect).

## Components

- **Toolbar** (unified title bar): 52 px, `TOOLBAR`, bottom hairline. On
  macOS the window's traffic lights are centred in it (an empty unified
  `NSToolbar`), content starts 80 px from the left on the first page header;
  the toolbar's empty area drags the window and a double-click zooms it (as
  the drag bar does now). The separate 28 px drag bar goes away.
- **Toolbar group** (transport, recording): 30 px tall, radius 7,
  `SURFACE_RAISED` with an inset `BORDER_STRONG` outline; 34 px wide icon
  buttons inside; 1 × 16 px `GRID_STRONG` dividers between subgroups.
- **Icon button**: transparent, 24 (panels) or 28 (toolbar) square, radius
  5–6, icon `TEXT_SECONDARY`, hover background `HOVER`.
- **Ghost button** ("New take", "New bus"): 22 px, radius 5, transparent,
  inset outline, 11 px text with a 10 px plus.
- **Pop-up button** (presets, interpretation, output, send): 22 px, radius
  5, `SURFACE_RAISED` + inset outline, 11 px text, an up-down chevron
  `TEXT_FAINT` on the right.
- **Primary action**: there is at most one per screen. In a toolbar it is an
  icon button like the others; in a dialog it is a filled `ACCENT` button
  (text `#1B0F0D`, 12 px semibold, 28 px, radius 6).
- **Segmented control**: 22 px (20 inside a lane bar), radius 6, background
  `#1C1C21` with an inset outline, 2 px padding; the selected segment is
  `#34343D`, radius 4, `TEXT_PRIMARY` medium; others transparent,
  `TEXT_SECONDARY`.
- **Tabs** (panel tabs, dock tabs, lane tabs): sentence-case text buttons,
  selected `CONTROL_ACTIVE` background radius 4–5 and `TEXT_PRIMARY`; a count
  may follow in `TEXT_FAINT` ("Notes 4"). Dock tabs are grouped:
  Summary | Vocal tract | Pitch · Vibrato · Range · Register | Spectrogram ·
  Power · LTAS · Timbre · Vowel · Dynamics, with Mixer at the right end.
- **Slider**: 12 px tall hit row; 2 px track `#2A2A31`; fill `#8A8A96`
  (`#3A3A43` when the owner is off); 10 px round knob `TEXT_PRIMARY` with a
  1 px drop shadow (8 px in track headers). Its value sits right-aligned in
  a 54 px column, 11 px tabular.
- **Parameter row**: 22 px; label (62 px column, 11 px `TEXT_SECONDARY`),
  slider, value.
- **Power button** (effect enable): 18 px circle with the power glyph; on:
  `ACCENT` at 16 % behind `ACCENT_BRIGHT`; off: `CONTROL` behind
  `TEXT_DISABLED`, and the effect's body at 55 % opacity.
- **Switch** (a setting on or off, in Settings): a 28 × 16 track, radius
  8, `CONTROL_ACTIVE` with an inset `BORDER_STRONG` outline while off,
  `ACCENT` while on (`SEGMENT_SELECTED` / `ACCENT_BRIGHT` under the
  pointer), and a 12 px `TEXT_PRIMARY` knob 2 px in at its left or right.
  Not Material's switch.
- **M / S / R**: 20–22 × 18–20, radius 4, 10 px semibold letters; off
  `CONTROL`/`TEXT_SECONDARY`; mute on and record on: `RECORD` at 20–24 % /
  `#F08A80`; solo on: `WARN` at 22 % / `#E3C46A`.
- **Pan knob**: 24–26 px; a 270° track `#2A2A31`, the value arc
  `TEXT_SECONDARY` from the centre, a 6.5 px cap with a `TEXT_PRIMARY`
  pointer; its value below at 10 px.
- **Fader** (mixer): 3 px slot `INSET`, a 21 × 12 cap (radius 3, a subtle
  vertical gradient `#4A4A54`→`#34343D`, a 1 px `TEXT_PRIMARY` centre line),
  4 px scale ticks; beside it 4 px meters (one per channel) on `INSET`,
  green to 70 %, `WARN` to 86 %, `RECORD` at the top. The dB value below in
  a 20 px `INSET` field.
- **Mixer strip**: 84 px wide (input 72, master 104), radius 6, `SURFACE`
  with an inset `#24242B` outline (selected `#202026` with `#4A4A54`); a 3 px
  track-colour cap; name 11 px medium; M/S; pan; fader + meters; dB field;
  output pop-up. Groups (input | tracks | buses | master) are separated by a
  1 px divider. The input strip's buttons are R (record) and I (input
  monitoring, lit as solo is); its knob is the monitor level, a level knob
  (the pan knob's look, its arc from the start of the track, its value
  below); its fader the record gain over the input's meter, its dB field
  "+12.0".
- **Effect module** (inspector): a 24 px header (drag handle dots, power
  button, 12 px medium name, a right-aligned 11 px `TEXT_FAINT` badge such
  as "GR −2.1 dB" or "Bypassed"), then its parameter rows; 8/14/12 px
  padding; 1 px separator between modules. The EQ graph is a 92 px well
  (`#0F0F12`, radius 6, inset outline), vertical decade lines `#1C1C22`,
  a 0 dB line, the curve 1.5 px `#E3A094` over a 10 % fill, band nodes
  7 px `TEXT_PRIMARY` with a dark ring, 9 px axis labels.
- **Track header**: 212 px; 3 px stripe in the track colour on its left
  edge; name 12 px medium, a "⋯" menu; M/S, an 8 px-knob volume slider, and
  20 px icon buttons. Selected: `PANEL_SELECTED`.
- **Section rows** in the arrange ("Accompaniment", "Vocal takes", "FX
  buses", "Master"): 30 px, 11 px semibold `TEXT_SECONDARY`, controls on the
  right (segmented control, ghost and pop-up buttons).
- **Drop zone** (an empty Accompaniment or Vocal takes section): a 52 px
  box inset 14 / 10, radius 6, a 1 px dashed outline (4 on, 3 off) in
  `BORDER_STRONG`, a 14 px icon and a 12 px line in `TEXT_FAINT`; while a
  file is dragged over it, the outline `ACCENT` at 70 %, the box `ACCENT`
  at 6 % and the line `ACCENT_BRIGHT`.
- **Take lanes** (a take folded out): a 28 px bar of lane tabs (each a
  toggle, lit while its lane shows), the lanes on `#0F0F12` wells with
  `BORDER_SUBTLE` lines between them; beside them, in the header column,
  each lane's cell (its name 11 px `TEXT_SECONDARY`, a readout 11 px
  `TEXT_FAINT`; the pitch score 18 px semibold), the first cell reaching up
  beside the tab bar.
- **Ruler**: 28 px, `#16161A`; major ticks every 10 s with 10 px labels
  `TEXT_FAINT` ("0:10"); minor 5 px ticks `#2A2A31`; the playhead marker a
  white 11 × 8 triangle; song end a 1 px `TEXT_DISABLED` line.
- **Pitch lane**: a `#0F0F12` well; semitone lines `#18181D`; target notes
  `#26303A` 5 px bars; the sung pitch a 1.6 px `IN_TUNE` line (off by more
  than the tolerance: `#F2A65A`, keeping its cent label); C labels 9 px.
- **Cards** (library, technique library): radius 8, `SURFACE` with an inset
  `BORDER` outline; hover `#1C1C21` with `#3A3A43` and a soft shadow. A
  library card has a 76 px `#0F0F12` band with the project's waveform in its
  colour (or "No recordings yet") and its length, then the title (13 px
  semibold) and a meta row (11 px: "Backing · 3 takes · 4 notes" in
  `TEXT_SECONDARY`, a relative date in `TEXT_FAINT`: Today, Yesterday,
  "Nov 24", then "Nov 24, 2025" in another year).
- **Dialogs, menus, tooltips**: `SURFACE` cards radius 10 (menus 8), inset
  `BORDER_STRONG` outline, a soft shadow; titles 13 px semibold; field
  labels sentence case 11 px `TEXT_SECONDARY`; buttons right-aligned:
  secondary (transparent + outline), primary filled `ACCENT`. The barrier is
  black at 50 %.
- **Update banner** (the library, under its toolbar): the full width,
  `ACCENT` at 8 % with a 1 px `BORDER` line under it, 8 / 32 px padding; a
  14 px icon, a 12 px medium `TEXT_PRIMARY` line (one line, ellipsized; two
  for a failure), then its buttons, 8 apart, and a 28 px icon button with a
  14 px cross ("Dismiss" tooltip). One state at a time:
  - *offer*: the `ACCENT_BRIGHT` up-arrow, "Vocara 0.4.0 is available.",
    "What's new" (secondary) and "Update now" (the screen's one primary,
    filled `ACCENT`, a download icon); where Vocara cannot replace itself, a
    single secondary "Download" instead (the dialog says why);
  - *downloading*: the download icon, "Downloading Vocara 0.4.0…", the
    count "12.3 MB of 41.2 MB" (11 px tabular `TEXT_SECONDARY`), a 160 × 4
    progress bar and a secondary "Cancel"; no dismiss;
  - *installing*: a 12 px `TEXT_SECONDARY` spinner and "Installing Vocara
    0.4.0… Vocara will relaunch."; nothing to press;
  - *failed*: the `WARN_TEXT` triangle, "Update failed: <why>", "Details"
    (secondary) and "Try again" (primary) while the release is on offer, and
    the dismiss.
- **Progress bar**: 4 px, radius 2, track `CONTROL_ACTIVE`, fill the
  neutral slider fill `#8A8A96` (progress is not a selection: no accent).
- **Update dialog** (480 wide, the dialog card): the 13 px semibold title
  "Vocara 0.4.0 is available", the release notes (12 px `TEXT_SECONDARY`, at
  most 200 px, scrolling), then by state: "Later" and "Update now"; while
  downloading, the line and count over a full-width progress bar with a
  secondary "Cancel" at its right, "Update now" disabled; installing, the
  spinner and "Installing… Vocara will relaunch."; failed, the `WARN_TEXT`
  triangle and "Update failed: <why>", then "Later", "Open Download Page"
  (secondary) and "Try again" (primary); where Vocara cannot update itself,
  "Vocara can't update itself here: <why>." over the release page's URL in a
  field well (selectable), then "Later" and "Open Download Page" (primary).
  Later, Escape and the barrier close it and leave a download running.
- **First-launch question** (a confirm dialog, the refresh icon in its
  accent badge): "Check for Vocara updates automatically?", its 12 px
  explanation, and "Don't check" / "Check automatically" (primary, not
  destructive). Asked once, only over the library.
- **Settings → About**: the "Check for updates" row's line follows an
  update under way ("Downloading Vocara 0.4.0… 12.3 MB of 41.2 MB",
  "Installing…", "Update failed: …"), and "Check automatically" is a switch
  row (clock icon, title over its 11 px explanation, the switch at the
  right).
- **Text fields**: 28 px (single line), radius 6, `INSET` `#0F0F12` with an
  inset outline; focused outline `ACCENT` at 60 %; 12 px text; placeholder
  `TEXT_FAINT`.
- **Lyrics**: rows of a 34 px time column (10 px tabular) and 13 px text
  with a 20 px line; the current line `#24242A` background radius 6, text
  white medium, its time `ACCENT_BRIGHT`; sung lines `TEXT_FAINT`, upcoming
  `#C4C4CE`.
- **Metric cards** (analysis summary): `SURFACE` radius 8, inset outline;
  11 px label, 18–20 px tabular value with an 11 px unit; a 4 px bar; a
  status dot only when it adds information.
- **Vocal tract** (dock tab; its mockups are the "Vocal tract" page of the
  design canvas): a 22 px header (12 px semibold title; the source, a 7 px
  `RECORD` dot with an 18 % halo and "Live from your microphone", or the
  take's colour dot and name; the 2D / 3D and Take / Live segmented
  controls), over a figure well: radius 10, an inset `#222228` outline, a
  soft radial light from `#15151A` a little above the middle to `#0E0E11`.
  The figure is drawn from centimetres at one uniform scale fitted to the
  well. 2D: the head's fill `#1B1B21`→`#17171C` (its crown fading out at the
  frame), outlines `BORDER_STRONG`; bone `#3C3C45`→`#2B2B32`; the parts that
  move coral (the tongue `#E9A99C`→`#B5665A` with a soft glow and a white
  highlight, the velum `#C47B6E`, the lips `#C27668`→`#E6A498`, edges
  `#E6A498`/`#F2B6AA`); the airway `IN_TUNE` at 32→12 % with a dotted
  centreline; the larynx `TRACK_PERIWINKLE`; the folds a white line with a
  halo that brightens while voiced. Callouts 11 px in two aligned columns,
  elbowed `#45454F` leaders and 2.3 px dots (moving parts `TEXT_PRIMARY` with
  `ACCENT_BRIGHT` dots, fixed ones `TEXT_SECONDARY` with `TEXT_FAINT` dots);
  a 10 px legend bottom-left, "Drawn to scale · adult model" bottom-right.
  3D: projected lines only — the head shell `#8A8A96` at 6–22 % by depth,
  the airway's rings `ACCENT_BRIGHT` brighter toward the lips (the glottis
  white) with a soft glow, tongue ribs `#E6A498`, the mandible
  `TEXT_SECONDARY`, the palate `TEXT_DISABLED`; a Side / ¾ / Front
  segmented control top-right. Resting (no voice), the figure holds its last
  shape, dimmed. Without an estimate the status says why, the figure
  resting: "Getting ready" (Live, while the model sets up for the voice),
  "Reading this take" (its shapes computing), "No analysis" (a take with
  nothing to read), "No take selected". Beside it a 404 px readout panel: a card (radius 10,
  `#18181C`, inset `#24242B`) of 30 px component rows (26–24 px in a short
  dock) — a 12 px `#C4C4CE` label, a 2 px `CONTROL_ACTIVE` track with 9 px
  `#5E5E6B` range ends, a 6 px band `ACCENT` at 22 % over the last two
  seconds, a 10 px `#F4EFEA` marker with a coral glow, a 12 px medium tabular
  value — then a vowel-map card, the fit (a dot and "Good fit" in `GOOD`,
  "Fair fit" in `WARN`, "Uncertain" in `TEXT_FAINT`), the formants (11 px
  tabular) and the caveat (11 px `TEXT_DISABLED`).

## What does not change

Layouts and their proportions, every feature, control and keyboard
shortcut, the data shown, the analysis algorithms, motion timings, and the
behaviour the tests pin (hit targets may grow, never shrink below 18 px).
