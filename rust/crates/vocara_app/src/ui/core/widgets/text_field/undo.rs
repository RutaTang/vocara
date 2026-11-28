//! The fields' undo: ⌘Z takes back the last edit and ⇧⌘Z puts it back, as
//! Flutter's `TextField` did (its `UndoHistory`) and a macOS field does.
//!
//! A run of typing is one step, as is a run of deleting: an edit of the same
//! kind as the last, within [`RUN_PAUSE`] of it, joins its step. Any other
//! edit (a paste, a cut), a pause, a move of the caret, an undo or a redo
//! starts a step of its own. Each field keeps its own history while it is on
//! screen, at most [`DEPTH`] steps, and forgets it when its text changes
//! from outside it (the app put another text in: another record's, a reset),
//! which no step leads back from.

use iced::time::{Duration, Instant};

/// How long a pause ends a run of typing (Flutter's `UndoHistory` throttle).
pub const RUN_PAUSE: Duration = Duration::from_millis(500);

/// How many steps a field keeps.
pub const DEPTH: usize = 100;

/// What kind of edit changed the text, for its runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Typed text (an input method's commit too).
    Typing,
    /// Backspace or Delete.
    Deleting,
    /// A paste, a cut: a step of its own.
    Other,
}

/// A field's text and its caret (or selection), as an undo puts them back.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot<C> {
    pub text: String,
    pub cursor: C,
}

/// A field's undo history (see the module docs).
#[derive(Debug, Clone)]
pub struct History<C> {
    undo: Vec<Snapshot<C>>,
    redo: Vec<Snapshot<C>>,
    /// The last edit's kind and time, while a run can go on from it.
    run: Option<(Kind, Instant)>,
    /// The text and caret the field's own edit, undo or redo last left,
    /// when known.
    left: Option<Snapshot<C>>,
}

impl<C> Default for History<C> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            run: None,
            left: None,
        }
    }
}

impl<C: Clone + PartialEq> History<C> {
    /// Takes in that an edit of `kind`, at `now`, changes the text from
    /// `before`: `before` is a step to go back to, unless the edit carries
    /// on the run of the last one (the caret where that edit left it). What
    /// was undone can no longer be redone.
    pub fn record(&mut self, before: Snapshot<C>, kind: Kind, now: Instant) {
        self.follow(&before.text);
        let moved = self
            .left
            .as_ref()
            .is_some_and(|left| left.cursor != before.cursor);
        let joins = !moved
            && self.run.is_some_and(|(last, at)| {
                last == kind
                    && kind != Kind::Other
                    && now.saturating_duration_since(at) <= RUN_PAUSE
            });
        if !joins {
            self.undo.push(before);
            if self.undo.len() > DEPTH {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.run = Some((kind, now));
    }

    /// Takes in the text and caret the field's own edit, undo or redo left.
    pub fn left(&mut self, after: Snapshot<C>) {
        self.left = Some(after);
    }

    /// Forgets every step when the text is no longer the one the field
    /// left: it came from outside the field.
    pub fn follow(&mut self, text: &str) {
        if self.left.as_ref().is_some_and(|left| left.text != text) {
            *self = Self::default();
        }
    }

    /// ⌘Z from `current`: the latest step whose text is not `current`'s (an
    /// edit that changed nothing, a Backspace at the start, left a step of
    /// the same text), which ⇧⌘Z then goes forward from.
    pub fn undo(&mut self, current: Snapshot<C>) -> Option<Snapshot<C>> {
        self.follow(&current.text);
        self.run = None;
        let back = pop_other(&mut self.undo, &current.text)?;
        self.redo.push(current);
        self.left(back.clone());
        Some(back)
    }

    /// ⇧⌘Z from `current`: the step the last undo went back from.
    pub fn redo(&mut self, current: Snapshot<C>) -> Option<Snapshot<C>> {
        self.follow(&current.text);
        self.run = None;
        let forward = pop_other(&mut self.redo, &current.text)?;
        self.undo.push(current);
        self.left(forward.clone());
        Some(forward)
    }
}

/// The last of `steps` whose text is not `text`, those after it dropped.
fn pop_other<C>(steps: &mut Vec<Snapshot<C>>, text: &str) -> Option<Snapshot<C>> {
    while let Some(step) = steps.pop() {
        if step.text != text {
            return Some(step);
        }
    }
    None
}

/// Whether a key press is ⌘Z (`Some(false)`) or ⇧⌘Z (`Some(true)`): its
/// latin letter (`Key::to_latin`; ⇧ makes it "Z") with ⌘ held.
pub fn undo_key(latin: Option<char>, modifiers: iced::keyboard::Modifiers) -> Option<bool> {
    match latin? {
        'z' | 'Z' if modifiers.command() => Some(modifiers.shift() || latin == Some('Z')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;

    fn at(text: &str) -> Snapshot<usize> {
        Snapshot {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    /// An edit from `from` to `to`, the caret at the end of each, as a
    /// field types it.
    fn typed(history: &mut History<usize>, from: &str, to: &str, kind: Kind, now: Instant) {
        history.record(at(from), kind, now);
        history.left(at(to));
    }

    #[test]
    fn a_typing_run_is_one_step_and_a_pause_or_a_paste_starts_another() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut h = History::default();
        typed(&mut h, "", "a", Kind::Typing, t0);
        typed(&mut h, "a", "ab", Kind::Typing, t0 + ms(100));
        typed(&mut h, "ab", "abc", Kind::Typing, t0 + ms(200));
        // A pause, then more.
        typed(&mut h, "abc", "abcd", Kind::Typing, t0 + ms(900));
        // A paste right after: a step of its own.
        typed(&mut h, "abcd", "abcd!", Kind::Other, t0 + ms(950));
        assert_eq!(h.undo(at("abcd!")).unwrap().text, "abcd");
        assert_eq!(h.undo(at("abcd")).unwrap().text, "abc");
        assert_eq!(h.undo(at("abc")).unwrap().text, "");
        assert_eq!(h.undo(at("")), None);
        // And forward again, step by step.
        assert_eq!(h.redo(at("")).unwrap().text, "abc");
        assert_eq!(h.redo(at("abc")).unwrap().text, "abcd");
        // A new edit drops what was undone.
        typed(&mut h, "abcd", "abcdX", Kind::Typing, t0 + ms(2000));
        assert_eq!(h.redo(at("abcdX")), None);
    }

    #[test]
    fn a_caret_move_ends_a_run_and_deleting_is_a_run_of_its_own() {
        let t0 = Instant::now();
        let mut h = History::default();
        typed(&mut h, "", "ab", Kind::Typing, t0);
        // The caret moved to the start, and "x" typed there.
        let start = |text: &str| Snapshot {
            text: text.to_string(),
            cursor: 0,
        };
        h.record(start("ab"), Kind::Typing, t0);
        h.left(Snapshot {
            text: "xab".into(),
            cursor: 1,
        });
        h.record(
            Snapshot {
                text: "xab".into(),
                cursor: 1,
            },
            Kind::Deleting,
            t0,
        );
        h.left(start("ab"));
        assert_eq!(h.undo(start("ab")).unwrap().text, "xab");
        assert_eq!(h.undo(at("xab")).unwrap().text, "ab");
        assert_eq!(h.undo(at("ab")).unwrap().text, "");
    }

    #[test]
    fn a_text_put_in_from_outside_forgets_the_history() {
        let t0 = Instant::now();
        let mut h = History::default();
        typed(&mut h, "", "first record", Kind::Typing, t0);
        // The app put another record's text in: nothing leads back from it.
        assert_eq!(h.undo(at("second record")), None);
        // An edit that changed nothing leaves no step to stop at.
        typed(&mut h, "", "a", Kind::Typing, t0);
        h.record(at("a"), Kind::Deleting, t0);
        h.left(at("a"));
        assert_eq!(h.undo(at("a")).unwrap().text, "");
    }

    #[test]
    fn command_z_undoes_and_shift_command_z_redoes() {
        assert_eq!(undo_key(Some('z'), Modifiers::COMMAND), Some(false));
        assert_eq!(
            undo_key(Some('z'), Modifiers::COMMAND | Modifiers::SHIFT),
            Some(true)
        );
        assert_eq!(undo_key(Some('Z'), Modifiers::COMMAND), Some(true));
        assert_eq!(undo_key(Some('z'), Modifiers::empty()), None);
        assert_eq!(undo_key(Some('x'), Modifiers::COMMAND), None);
    }
}
