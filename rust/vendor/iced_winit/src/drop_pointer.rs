//! vocara: where the pointer is while a file is dragged in from another
//! app (Finder), on macOS.
//!
//! winit reports no `CursorMoved` during a drag that started outside the
//! window, so the cursor iced knew was where the pointer last moved over the
//! window before the drag came in, or none (it left the window): a drop was
//! judged there, and the app's drop targets (which take a drop only where a
//! click would land) refused it. The event loop asks [`position`] when a
//! file hovers over or is dropped on the window, and moves the cursor there
//! first, as if the pointer had moved.

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
    let ns_window = view.window()?;
    let in_window = ns_window.mouseLocationOutsideOfEventStream();
    let in_view = view.convertPoint_fromView(in_window, None);
    let bounds = view.bounds();
    from_view(
        (in_view.x, in_view.y),
        (bounds.size.width, bounds.size.height),
        view.isFlipped(),
        window.scale_factor(),
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
    use super::from_view;

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
