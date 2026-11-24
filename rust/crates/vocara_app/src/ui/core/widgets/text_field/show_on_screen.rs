//! `RenderObject.showOnScreen` as a text field asked it of the scroll view
//! around it: after the field reveals its caret in its own text
//! (`EditableText._scheduleShowCaretOnScreen`), the view around it shows
//! the caret with the field's `scrollPadding` around it (20 on every side,
//! 24 below once the selection overlay was up, which on macOS it was from
//! the first selection), over the same 100 ms on `Curves.fastOutSlowIn`;
//! after a keyboard move or a drag of the selection (`bringIntoView`) it
//! shows the bare caret at once.
//!
//! A field cannot reach the views above it in iced, so it leaves its
//! request in its state and counts it ([`ask`]); the app's vertical scroll
//! views (`fading_scrollbar`) look for requests among their content after
//! each event they pass to it, and carry them out as
//! `RenderViewportBase.showInViewport` did: the least scroll that shows
//! the rectangle, clamped to the view's range (`ScrollPosition.moveTo`).
//! A view inside another takes the request alone (Flutter's went on to the
//! outer views); no screen has a field in nested views.
//!
//! The single-line field asks at every reveal ([`input::scroll`]). The
//! multi-line one asks only when the keyboard's traversal focuses it: its
//! edits and moves scroll its own text as iced's editor does, and do not
//! yet ask the view around it to follow its caret, as Flutter's did.

use std::any::Any;
use std::cell::Cell;

use iced::advanced::widget::{Id, Operation};
use iced::{Padding, Rectangle};

use super::{editor, input};

/// `EditableText.scrollPadding`'s default, with the bottom the selection
/// overlay's handles asked for (`max(handleCenter + kMinInteractiveDimension
/// / 2, scrollPadding.bottom)`, the desktop handles having no size).
pub const CARET_PADDING: Padding = Padding {
    top: 20.0,
    right: 20.0,
    bottom: 24.0,
    left: 20.0,
};

/// A rectangle a field asks the view around it to show, in layout
/// coordinates, at once or over the caret animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Request {
    pub rect: Rectangle,
    pub animate: bool,
}

thread_local! {
    /// How many requests fields have made on this thread.
    static ASKED: Cell<u64> = const { Cell::new(0) };
}

/// Counts a request a field has just left in its state.
pub fn ask() {
    ASKED.with(|asked| asked.set(asked.get().wrapping_add(1)));
}

/// How many requests fields have made: a view whose last look was at
/// another count looks again.
pub fn asked() -> u64 {
    ASKED.with(Cell::get)
}

/// Takes the requests of the fields it traverses, in order.
#[derive(Default)]
pub struct Take(pub Vec<Request>);

impl Operation for Take {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Any) {
        if let Some(state) = state.downcast_mut::<input::State>() {
            self.0.append(&mut state.requests);
        } else if let Some(state) = state.downcast_mut::<editor::State>() {
            self.0.append(&mut state.requests);
        }
    }
}

/// `showInViewport` for a vertical view `viewport` tall whose content
/// starts at `content_top` and is scrolled `pixels` down of `range`: where
/// it scrolls to show `rect`, none if it shows already.
pub fn target(
    viewport: f32,
    content_top: f32,
    pixels: f32,
    range: f32,
    rect: Rectangle,
) -> Option<f32> {
    // The offsets that put the rect's top at the view's top (leading) and
    // its bottom at the view's bottom (trailing).
    let leading = rect.y - content_top;
    let trailing = rect.y + rect.height - content_top - viewport;
    // `RevealedOffset.clampOffset`.
    let (smaller, larger) = if leading < trailing {
        (leading, trailing)
    } else {
        (trailing, leading)
    };
    let to = if pixels > larger {
        larger
    } else if pixels < smaller {
        smaller
    } else {
        return None;
    };
    Some(to.clamp(0.0, range.max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, Size};

    fn rect(y: f32, height: f32) -> Rectangle {
        Rectangle::new(Point::new(0.0, y), Size::new(10.0, height))
    }

    #[test]
    fn the_view_scrolls_the_least_that_shows_the_rect() {
        // A 100 px view over content from 50, scrolled 200 of 500.
        let at = |r| target(100.0, 50.0, 200.0, 500.0, r);
        assert_eq!(at(rect(300.0, 20.0)), None, "in view");
        assert_eq!(
            at(rect(200.0, 20.0)),
            Some(150.0),
            "above: its top at the top"
        );
        assert_eq!(
            at(rect(400.0, 20.0)),
            Some(270.0),
            "below: its bottom at the bottom"
        );
        assert_eq!(at(rect(0.0, 20.0)), Some(0.0), "no further than the start");
        // Taller than the view and over it: stays.
        assert_eq!(at(rect(200.0, 300.0)), None);
    }
}
