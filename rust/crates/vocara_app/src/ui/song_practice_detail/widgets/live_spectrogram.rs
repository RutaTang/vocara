//! Port of `ui/song_practice_detail/widgets/live_spectrogram.dart`: the
//! scrolling live spectrogram driven by the [`MonitorService`] — the mic's
//! rolling window of columns, oldest on the left, under the frequency grid,
//! with a hint until the first column arrives.
//!
//! Dart's `_LiveSpectrogramState` listened to the monitor and rebuilt its
//! image at most once per frame (a post-frame callback coalescing the
//! monitor's notifications). Here the canvas keeps the raster and the image
//! in its widget-tree state (the counterpart of that `State`) and brings the
//! image up to date when it draws — at most once per frame — and only when
//! the monitor's [revision](MonitorState::revision) moved since the image
//! was made. Its pixels come from a [`LiveColumnsRaster`], which colours only
//! the columns that arrived since the last frame (unless the window's
//! ceiling moved): the whole 720-column window was recoloured on every frame
//! before.
//!
//! An iced image handle is made from its pixels at once, so Dart's
//! generation counter — which dropped an asynchronous decode that finished
//! after a newer one started, or after the monitor was reset to no columns —
//! has nothing left to guard: the image is always the window the monitor
//! held when the frame was drawn.
//!
//! [`MonitorState::revision`]: vocara_services::monitor_service::MonitorState::revision

use std::cell::RefCell;

use crate::ui::core::text;
use iced::widget::canvas::{self, Geometry};
use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{canvas as canvas_widget, center, container, stack, Space};
use iced::{mouse, Element, Length, Rectangle, Renderer, Theme};
use vocara_services::monitor_service::{LiveColumn, MonitorService};

use crate::ui::core::theme::{fill, Studio};
use crate::ui::core::widgets::painter::KeyedCache;
use crate::ui::core::widgets::spectrogram_painter::{
    decode_rgba_image, FrequencyGridPainter, LiveColumnsRaster,
};

#[cfg(test)]
pub(super) mod tests;

/// The hint shown until the monitor has a column.
pub const LIVE_SPECTROGRAM_HINT: &str = "Sing to see your voiceprint…";

/// `LiveSpectrogram(monitor: ...)`: the inset well, the rolling image
/// stretched over it, the frequency grid on top, and — while the monitor has
/// no column — [`LIVE_SPECTROGRAM_HINT`]. It takes no input (Dart wrapped
/// the grid in `IgnorePointer`; these canvases have no `update`).
pub fn live_spectrogram<'a, M: 'a>(monitor: &'a MonitorService) -> Element<'a, M> {
    // Read and let go: the image layer reads the monitor again when it draws.
    let (empty, max_hz) = {
        let m = monitor.read();
        (m.columns().is_empty(), m.max_hz())
    };
    let mut layers = stack![
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(Studio::INSET)),
        canvas_widget(LivePainter { monitor })
            .width(Length::Fill)
            .height(Length::Fill),
        canvas_widget(FrequencyGridPainter::new(max_hz))
            .width(Length::Fill)
            .height(Length::Fill),
    ];
    // Pushed last, so the layers under it keep their place (and the image
    // its state) as the hint comes and goes.
    if empty {
        layers = layers.push(center(
            text(LIVE_SPECTROGRAM_HINT)
                .size(13)
                .color(Studio::TEXT_FAINT),
        ));
    }
    layers.width(Length::Fill).height(Length::Fill).into()
}

/// What a live image was made from: the monitor (by address) and its
/// revision then.
type ImageKey = (usize, u64);

/// The live image's state, kept in the widget tree across views (Dart's
/// `_LiveSpectrogramState`): the raster, the image it last made, and the
/// geometry drawing it.
#[derive(Default)]
struct LiveImageState {
    raster: RefCell<LiveColumnsRaster>,
    /// The image and the window it shows; `None` inside when the window was
    /// empty (Dart cleared its image when the monitor reset to no columns).
    image: RefCell<Option<(ImageKey, Option<Handle>)>>,
    cache: KeyedCache<ImageKey>,
}

#[cfg(test)]
thread_local! {
    /// How many times a live image was rebuilt from the monitor, on this
    /// thread.
    static IMAGE_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `_LivePainter`: the rolling window's image, stretched over the box
/// (`FilterQuality.medium`: linear filtering).
struct LivePainter<'a> {
    monitor: &'a MonitorService,
}

impl LivePainter<'_> {
    /// Brings `state`'s image up to the monitor's current window, if the
    /// monitor moved since; returns the window's key.
    fn sync(&self, state: &LiveImageState) -> ImageKey {
        let address = std::ptr::from_ref(self.monitor) as usize;
        // One read: the revision, and — only when it moved — a snapshot of
        // both lists (same length, evicted in tandem), so the voicing mask
        // stays aligned with the columns. The colouring runs after the lock
        // is let go, so the feeder does not wait on it.
        let (key, snapshot) = {
            let m = self.monitor.read();
            let key = (address, m.revision());
            let current = state.image.borrow().as_ref().map(|(k, _)| *k);
            let snapshot = (current != Some(key))
                .then(|| (m.columns().to_vec(), m.active_columns().to_vec(), m.bins()));
            (key, snapshot)
        };
        if let Some((columns, active, bins)) = snapshot {
            #[cfg(test)]
            IMAGE_REBUILDS.with(|n| n.set(n.get() + 1));
            *state.image.borrow_mut() = Some((key, self.image_of(state, &columns, &active, bins)));
        }
        key
    }

    /// The image of a window: none for an empty one (the monitor was reset
    /// to no columns).
    fn image_of(
        &self,
        state: &LiveImageState,
        columns: &[LiveColumn],
        active: &[u8],
        bins: usize,
    ) -> Option<Handle> {
        if columns.is_empty() {
            return None;
        }
        let pixels = state
            .raster
            .borrow_mut()
            .update(columns, bins, Some(active));
        Some(decode_rgba_image(pixels, columns.len(), bins))
    }
}

impl<M> canvas::Program<M> for LivePainter<'_> {
    type State = LiveImageState;

    fn draw(
        &self,
        state: &LiveImageState,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let key = self.sync(state);
        let image = state
            .image
            .borrow()
            .as_ref()
            .and_then(|(_, image)| image.clone());
        vec![state.cache.draw(renderer, bounds.size(), key, |frame| {
            if let Some(image) = image {
                frame.draw_image(
                    Rectangle::with_size(bounds.size()),
                    canvas::Image::new(image).filter_method(FilterMethod::Linear),
                );
            }
        })]
    }
}
