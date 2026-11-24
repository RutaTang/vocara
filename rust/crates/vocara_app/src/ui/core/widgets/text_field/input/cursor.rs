//! The field's cursor: iced_widget 0.14.2's `text_input::cursor`, as it is
//! (its moves are crate-private there).

use iced::widget::text_input::Value;

/// The cursor of a [`TextInput`](super::TextInput).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Cursor {
    state: State,
}

/// The state of a [`Cursor`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum State {
    /// A caret at an index.
    Index(usize),
    /// A selection, from `start` (its base) to `end` (its extent).
    Selection { start: usize, end: usize },
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor {
            state: State::Index(0),
        }
    }
}

impl Cursor {
    /// The cursor's state in `value`.
    pub fn state(&self, value: &Value) -> State {
        match self.state {
            State::Index(index) => State::Index(index.min(value.len())),
            State::Selection { start, end } => {
                let start = start.min(value.len());
                let end = end.min(value.len());

                if start == end {
                    State::Index(start)
                } else {
                    State::Selection { start, end }
                }
            }
        }
    }

    /// The selection in `value`, lower end first, if not collapsed.
    pub fn selection(&self, value: &Value) -> Option<(usize, usize)> {
        match self.state(value) {
            State::Selection { start, end } => Some((start.min(end), start.max(end))),
            State::Index(_) => None,
        }
    }

    pub(super) fn move_to(&mut self, position: usize) {
        self.state = State::Index(position);
    }

    pub(super) fn move_right(&mut self, value: &Value) {
        self.move_right_by_amount(value, 1);
    }

    pub(super) fn move_right_by_words(&mut self, value: &Value) {
        self.move_to(value.next_end_of_word(self.right(value)));
    }

    pub(super) fn move_right_by_amount(&mut self, value: &Value, amount: usize) {
        match self.state(value) {
            State::Index(index) => {
                self.move_to(index.saturating_add(amount).min(value.len()));
            }
            State::Selection { start, end } => self.move_to(end.max(start)),
        }
    }

    pub(super) fn move_left(&mut self, value: &Value) {
        match self.state(value) {
            State::Index(index) if index > 0 => self.move_to(index - 1),
            State::Selection { start, end } => self.move_to(start.min(end)),
            State::Index(_) => self.move_to(0),
        }
    }

    pub(super) fn move_left_by_words(&mut self, value: &Value) {
        self.move_to(value.previous_start_of_word(self.left(value)));
    }

    pub(super) fn select_range(&mut self, start: usize, end: usize) {
        if start == end {
            self.state = State::Index(start);
        } else {
            self.state = State::Selection { start, end };
        }
    }

    pub(super) fn select_left(&mut self, value: &Value) {
        match self.state(value) {
            State::Index(index) if index > 0 => {
                self.select_range(index, index - 1);
            }
            State::Selection { start, end } if end > 0 => {
                self.select_range(start, end - 1);
            }
            _ => {}
        }
    }

    pub(super) fn select_right(&mut self, value: &Value) {
        match self.state(value) {
            State::Index(index) if index < value.len() => {
                self.select_range(index, index + 1);
            }
            State::Selection { start, end } if end < value.len() => {
                self.select_range(start, end + 1);
            }
            _ => {}
        }
    }

    pub(super) fn select_left_by_words(&mut self, value: &Value) {
        match self.state(value) {
            State::Index(index) => {
                self.select_range(index, value.previous_start_of_word(index));
            }
            State::Selection { start, end } => {
                self.select_range(start, value.previous_start_of_word(end));
            }
        }
    }

    pub(super) fn select_right_by_words(&mut self, value: &Value) {
        match self.state(value) {
            State::Index(index) => {
                self.select_range(index, value.next_end_of_word(index));
            }
            State::Selection { start, end } => {
                self.select_range(start, value.next_end_of_word(end));
            }
        }
    }

    pub(super) fn select_all(&mut self, value: &Value) {
        self.select_range(0, value.len());
    }

    pub(super) fn start(&self, value: &Value) -> usize {
        let start = match self.state {
            State::Index(index) => index,
            State::Selection { start, .. } => start,
        };

        start.min(value.len())
    }

    pub(super) fn end(&self, value: &Value) -> usize {
        let end = match self.state {
            State::Index(index) => index,
            State::Selection { end, .. } => end,
        };

        end.min(value.len())
    }

    fn left(&self, value: &Value) -> usize {
        match self.state(value) {
            State::Index(index) => index,
            State::Selection { start, end } => start.min(end),
        }
    }

    fn right(&self, value: &Value) -> usize {
        match self.state(value) {
            State::Index(index) => index,
            State::Selection { start, end } => start.max(end),
        }
    }
}
