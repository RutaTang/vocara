//! vocara: where the pointer is while a file is dragged in from another
//! app (Finder), on macOS.
//!
//! winit reports no `CursorMoved` during a drag that started outside the
//! window, so the cursor iced knew was where the pointer last moved over the
//! window before the drag came in, or none (it left the window): a drop was
//! judged there, and the app's drop targets (which take a drop only where a
//! click would land) refused it. The event loop asks [`position`] when a
//! file hovers over or is dropped on the window, and moves the cursor there
//! first, as if the pointer had moved ([`moved`]): only where it is not
//! already. winit reports each file of a drop on its own, and a move before
//! each but the first (to where the cursor was) ended the drop in the app's
//! targets, which split one drop of several files into one drop per file.
//!
//! In between, AppKit tells the window where the drag is
//! (`draggingUpdated:`), which winit's delegate does not implement: the
//! hover was judged where the drag came in, and a drop zone lit up only if
//! the drag entered over it. So while a file hovers, the event loop reads
//! the pointer again about 30 times a second and moves the cursor where it
//! changed ([`Drag`]). When the drag leaves the window (or is cancelled), the
//! cursor leaves too: the end of a drag is no pointer event, and a hover or
//! a tooltip the moves brought out stayed where the pointer last was.

use std::time::Duration;

use winit::dpi::PhysicalPosition;
use winit::event::WindowEvent;

use crate::core::time::Instant;

/// How often the pointer is read while a file hovers over a window: about
/// 30 times a second.
pub const READ_EVERY: Duration = Duration::from_millis(33);

/// What the event loop does around one of a window's events from winit,
/// for a drag in from another app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Around {
    /// Nothing more.
    Nothing,
    /// Reads the pointer and moves the cursor there first: a file hovers or
    /// is dropped.
    MoveFirst,
    /// The cursor leaves the window after it: the drag left it.
    LeaveAfter,
}

/// A window's drag in from another app: while a file hovers over it, when
/// the pointer is read next.
#[derive(Debug, Default)]
pub struct Drag {
    next_read: Option<Instant>,
}

impl Drag {
    /// Follows `event`, one of the window's from winit, at `now`: a file
    /// hovering starts the reads; a drop, the drag leaving, or the pointer
    /// moving or clicking by itself (which winit reports only once no drag
    /// is under way) ends them.
    pub fn event(&mut self, event: &WindowEvent, now: Instant) -> Around {
        match event {
            WindowEvent::HoveredFile(_) => {
                self.next_read = Some(now + READ_EVERY);
                Around::MoveFirst
            }
            WindowEvent::DroppedFile(_) => {
                self.next_read = None;
                Around::MoveFirst
            }
            WindowEvent::HoveredFileCancelled => {
                self.next_read = None;
                Around::LeaveAfter
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. } => {
                self.next_read = None;
                Around::Nothing
            }
            _ => Around::Nothing,
        }
    }

    /// Whether the pointer is read at `now`: a file hovers and its read is
    /// due. The next one is due [`READ_EVERY`] later.
    pub fn due(&mut self, now: Instant) -> bool {
        match self.next_read {
            Some(at) if at <= now => {
                self.next_read = Some(now + READ_EVERY);
                true
            }
            _ => false,
        }
    }

    /// When the pointer is read next, while a file hovers.
    pub fn next_read(&self) -> Option<Instant> {
        self.next_read
    }
}

/// When the event loop wakes next: at `redraw` (a window's next frame) or
/// at the next read of a drag (`read`), whichever comes first.
pub fn wake_at(
    redraw: Option<Instant>,
    read: Option<Instant>,
) -> Option<Instant> {
    match (redraw, read) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Where the cursor moves to for the pointer `read` over the window, from
/// where the window has it (`cursor`): `None` when the pointer is where
/// the cursor is already, or was not read (off the content).
pub fn moved(
    cursor: Option<PhysicalPosition<f64>>,
    read: Option<PhysicalPosition<f64>>,
) -> Option<PhysicalPosition<f64>> {
    read.filter(|position| cursor != Some(*position))
}

/// winit's event for the pointer moving to `position`. A dummy device id
/// only names the device; nothing reads it back.
pub fn cursor_moved(position: PhysicalPosition<f64>) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position,
    }
}

/// winit's event for the pointer leaving the window.
pub fn cursor_left() -> WindowEvent {
    WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    }
}

/// The pointer's position over `window`'s content, in physical pixels from
/// its top left, read from AppKit; `None` off macOS, off the main thread, or
/// with the pointer outside the content.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn position(
    window: &winit::window::Window,
) -> Option<winit::dpi::PhysicalPosition<f64>> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSView;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let _main_thread = MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: an AppKit window handle's `ns_view` is the live `NSView`
    // winit made for this window, which the window keeps alive; it is read
    // on the main thread, as AppKit requires.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    position_in(view)
}

/// The pointer's position over `view` (a window's content view), in
/// physical pixels from its top left; `None` with the pointer outside it or
/// the view in no window. Main thread only, as AppKit is (the caller's
/// view proves it is there). Also read by `drag_operation`, from AppKit's
/// own drag callback.
#[cfg(target_os = "macos")]
pub fn position_in(
    view: &objc2_app_kit::NSView,
) -> Option<winit::dpi::PhysicalPosition<f64>> {
    let ns_window = view.window()?;
    let in_window = ns_window.mouseLocationOutsideOfEventStream();
    let in_view = view.convertPoint_fromView(in_window, None);
    let bounds = view.bounds();
    from_view(
        (in_view.x, in_view.y),
        (bounds.size.width, bounds.size.height),
        view.isFlipped(),
        ns_window.backingScaleFactor(),
    )
    .map(|(x, y)| winit::dpi::PhysicalPosition::new(x, y))
}

/// Off macOS winit reports the pointer during a drag itself.
#[cfg(not(target_os = "macos"))]
pub fn position(
    _window: &winit::window::Window,
) -> Option<winit::dpi::PhysicalPosition<f64>> {
    None
}

/// A point in a view's coordinates (`flipped`: y runs down, else up from
/// the bottom), `size` points large, as physical pixels from its top left
/// at `scale`; `None` outside it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn from_view(
    point: (f64, f64),
    size: (f64, f64),
    flipped: bool,
    scale: f64,
) -> Option<(f64, f64)> {
    let (x, y) = point;
    let y = if flipped { y } else { size.1 - y };
    if !(0.0..=size.0).contains(&x) || !(0.0..=size.1).contains(&y) {
        return None;
    }
    Some((x * scale, y * scale))
}

#[cfg(test)]
mod tests {
    use super::{
        Around, Drag, Instant, PhysicalPosition, READ_EVERY, WindowEvent,
        cursor_left, cursor_moved, from_view, moved, wake_at,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    fn hovered() -> WindowEvent {
        WindowEvent::HoveredFile(PathBuf::from("/a.wav"))
    }

    fn dropped() -> WindowEvent {
        WindowEvent::DroppedFile(PathBuf::from("/a.wav"))
    }

    #[test]
    fn the_pointer_is_read_while_a_file_hovers() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut drag = Drag::default();
        assert!(!drag.due(t0), "no drag, no read");
        assert_eq!(drag.next_read(), None);
        assert_eq!(drag.event(&hovered(), t0), Around::MoveFirst);
        // The next read is due a 30th of a second on, then each one after.
        assert_eq!(drag.next_read(), Some(t0 + READ_EVERY));
        assert!(!drag.due(ms(10)));
        assert!(drag.due(ms(33)));
        assert_eq!(drag.next_read(), Some(ms(66)));
        assert!(!drag.due(ms(40)), "once per period");
        // A late wake reads once and counts on from then.
        assert!(drag.due(ms(100)));
        assert_eq!(drag.next_read(), Some(ms(133)));
        // A second file of the same drag changes nothing but the next read.
        assert_eq!(drag.event(&hovered(), ms(110)), Around::MoveFirst);
        assert!(drag.due(ms(143)));
    }

    #[test]
    fn a_drop_or_the_drag_leaving_ends_the_reads() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(1);
        let mut drag = Drag::default();
        let _ = drag.event(&hovered(), t0);
        assert_eq!(drag.event(&dropped(), t0), Around::MoveFirst);
        assert!(!drag.due(later));
        assert_eq!(drag.next_read(), None);

        let _ = drag.event(&hovered(), t0);
        assert_eq!(
            drag.event(&WindowEvent::HoveredFileCancelled, t0),
            Around::LeaveAfter
        );
        assert!(!drag.due(later));

        // winit reports the pointer itself once no drag is under way: a
        // drag whose end was never reported stops being read.
        let _ = drag.event(&hovered(), t0);
        let moved = cursor_moved(PhysicalPosition::new(1.0, 1.0));
        assert_eq!(drag.event(&moved, t0), Around::Nothing);
        assert!(!drag.due(later));
        // And the window's other events leave the reads alone.
        let _ = drag.event(&hovered(), t0);
        assert_eq!(drag.event(&cursor_left(), t0), Around::Nothing);
        assert_eq!(
            drag.event(&WindowEvent::Focused(true), t0),
            Around::Nothing
        );
        assert!(drag.due(later));
    }

    #[test]
    fn the_loop_wakes_for_the_first_of_a_frame_and_a_read() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(5);
        assert_eq!(wake_at(None, None), None);
        assert_eq!(wake_at(Some(t1), None), Some(t1));
        assert_eq!(wake_at(None, Some(t1)), Some(t1));
        assert_eq!(wake_at(Some(t1), Some(t0)), Some(t0));
        assert_eq!(wake_at(Some(t0), Some(t1)), Some(t0));
    }

    #[test]
    fn the_cursor_moves_only_to_where_it_is_not() {
        let at = |x, y| PhysicalPosition::new(x, y);
        // A drop's first file: the cursor was where the drag came in.
        assert_eq!(
            moved(Some(at(10.0, 10.0)), Some(at(300.0, 200.0))),
            Some(at(300.0, 200.0))
        );
        assert_eq!(moved(None, Some(at(300.0, 200.0))), Some(at(300.0, 200.0)));
        // Its next files: the cursor is there already.
        assert_eq!(moved(Some(at(300.0, 200.0)), Some(at(300.0, 200.0))), None);
        // The pointer off the content: the cursor stays.
        assert_eq!(moved(Some(at(300.0, 200.0)), None), None);
    }

    #[test]
    fn a_point_in_an_unflipped_view_is_measured_from_its_top() {
        // AppKit's y runs up from the bottom: 100 up a 600 tall view is
        // 500 down; at 2x, 1000 pixels down.
        assert_eq!(
            from_view((300.0, 100.0), (800.0, 600.0), false, 2.0),
            Some((600.0, 1000.0))
        );
        assert_eq!(
            from_view((300.0, 100.0), (800.0, 600.0), true, 1.0),
            Some((300.0, 100.0))
        );
    }

    #[test]
    fn a_point_outside_the_view_is_none() {
        assert_eq!(from_view((-1.0, 100.0), (800.0, 600.0), false, 1.0), None);
        assert_eq!(from_view((10.0, 601.0), (800.0, 600.0), false, 1.0), None);
        assert_eq!(from_view((801.0, 10.0), (800.0, 600.0), true, 1.0), None);
    }
}
