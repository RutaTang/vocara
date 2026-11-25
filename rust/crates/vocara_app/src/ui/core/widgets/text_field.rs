//! The app's text fields, as Flutter's macOS `TextField` and `SelectableText`
//! (under forui's Material theme) drew them and took the mouse: every Dart
//! field is a `TextField(cursorColor: Studio.accent, decoration:
//! InputDecoration(isCollapsed: true, border: InputBorder.none, ...))`, and
//! the update dialog's address is a `SelectableText`.
//!
//! iced's `text_input` and `text_editor` cannot draw Flutter's caret or show
//! its context menu: the caret is a 1 px square bar in the value's color,
//! drawn under the text, and the widgets keep their cursor, focus and blink
//! in private state, so nothing outside them can draw one in its place or
//! select the word under a right-click. So both are forked here from
//! iced_widget 0.14.2 (MIT, the version the workspace pins), with their
//! keyboard, mouse, clipboard, input method and selection handling kept as
//! they are, but where they differed from Flutter's as listed below:
//!
//! - [`input`]: the single-line field (`text_input`), over a `&str`, Return
//!   submitting;
//! - [`editor`]: the multi-line field (`text_editor`), over a [`Content`],
//!   which also draws `SelectableText` ([`TextEditor::read_only`]).
//!
//! What they do as Flutter did, where iced's did not:
//!
//! - **The caret** ([`caret`]): `RenderEditable`'s on macOS. 2 px wide
//!   (`cursorWidth`), its ends rounded (`cursorRadius` 2), in the field's
//!   `cursorColor` (every Dart field set `Studio.accent`; the builders'
//!   `caret_color`); one line tall and 1 px more above and below (the
//!   iOS/macOS `_caretPrototype`), centred on the line; 2 device pixels
//!   left of the glyph edge (`cursorOffset: iOSHorizontalOffset /
//!   devicePixelRatio`) but never left of the text; its top left on a
//!   device pixel (`_snapToPhysicalPixel`); painted over the text
//!   (`paintCursorAboveText`). It blinks as `EditableText` did with
//!   `cursorOpacityAnimates: false` (macOS): shown for 500 ms, hidden for
//!   500 ms (`_kCursorBlinkHalfPeriod`), shown afresh whenever the text,
//!   the selection or the focus changes, and only while the selection is
//!   collapsed and the window is focused. `SelectableText` shows none
//!   (`showCursor: false`).
//! - **The context menu** ([`toolbar`]): a right-click selects the word
//!   under it (unless it landed on the selection of the focused field), or
//!   puts the caret at the end when it is past the text, and opens the
//!   desktop Cupertino toolbar there (`TextSelectionGestureDetectorBuilder.
//!   onSecondaryTap` on macOS), with the items `EditableText` offered on
//!   macOS: Cut and Copy with a selection, Paste when the clipboard holds
//!   text, none of them in a read-only field but Copy, and no Select All
//!   (`selectAllEnabled` is false on macOS).
//! - **The caret margin**: a multi-line field wraps its text 3 px short of
//!   its width (`_kCaretGap + cursorWidth`), as `RenderEditable` laid it
//!   out, leaving the caret room at the end of a full line.
//! - **The single-line scroll** ([`input::scroll`]): text wider than the
//!   field scrolls only as far as shows the caret (`_showCaretOnScreen`),
//!   over 100 ms on `Curves.fastOutSlowIn` after an edit, a click or the
//!   focus coming, at once for a keyboard move or a drag of the selection;
//!   it stays put while the field is unfocused, Home and End only scroll
//!   on macOS, a scroll signal across scrolls it, and a trackpad's
//!   two-finger swipe drags and flings it as its `Scrollable`'s horizontal
//!   drag did, against a scroll view's vertical one around it. iced kept
//!   the caret 5 px in from the right edge as it moved, and an unfocused
//!   field at its start.
//! - **Cut** (Cmd+X) cuts nothing without a selection, where iced's
//!   deleted the character after the caret.
//! - **Tab** moves the focus to the next field of its focus scope
//!   ([`focus_traversal`](super::focus_traversal)), which a single-line
//!   field takes by selecting all its text; a disabled field takes none.
//!   Both report their state to it (`Operation::custom`).
//!
//! The device pixel ratio the caret needs is the window's scale factor,
//! which the app hands over when its window opens and the fields take
//! themselves when it changes ([`set_scale_factor`]).

pub mod caret;
pub mod editor;
pub mod input;
pub mod show_on_screen;
pub mod toolbar;

pub use caret::set_scale_factor;
pub use editor::{text_editor, Content, TextEditor};
pub use input::{text_input, TextInput};

/// The caret color of a field that sets none: `TextField`'s macOS
/// `cursorColor` fallback, `CupertinoTheme.of(context).primaryColor`, which
/// under a Material theme is its `colorScheme.primary` (zinc's).
pub const DEFAULT_CARET: iced::Color = crate::ui::core::theme::Zinc::PRIMARY;

#[cfg(test)]
mod tests;
