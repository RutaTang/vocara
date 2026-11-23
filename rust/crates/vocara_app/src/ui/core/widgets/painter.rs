//! Shared plumbing for the painters: a canvas cache cleared when the
//! painter's inputs change (Flutter's `shouldRepaint`), and identity
//! comparison for shared buffers (Dart compared lists by identity).

use std::cell::RefCell;
use std::sync::Arc;

use iced::widget::canvas::{Cache, Frame, Geometry};
use iced::{Renderer, Size};

/// A shared buffer compared by identity, not contents: Dart's
/// `old.peaks != peaks`. Holding the `Arc` keeps the buffer alive while it is
/// the cached key, so its address cannot be reused by a different buffer.
#[derive(Debug)]
pub struct Same<T: ?Sized>(pub Arc<T>);

impl<T: ?Sized> Clone for Same<T> {
    fn clone(&self) -> Self {
        Same(Arc::clone(&self.0))
    }
}

impl<T: ?Sized> PartialEq for Same<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A canvas [`Cache`] that is cleared whenever the key it was drawn for
/// changes — the iced counterpart of `CustomPainter.shouldRepaint`. (The
/// cache also redraws by itself when the canvas size changes.)
pub struct KeyedCache<K> {
    cache: Cache,
    key: RefCell<Option<K>>,
}

impl<K> Default for KeyedCache<K> {
    fn default() -> Self {
        Self {
            cache: Cache::default(),
            key: RefCell::new(None),
        }
    }
}

impl<K: PartialEq> KeyedCache<K> {
    /// Draws through the cache, re-running `draw` only when `key` differs
    /// from the last one or the size changed.
    pub fn draw(
        &self,
        renderer: &Renderer,
        size: Size,
        key: K,
        draw: impl FnOnce(&mut Frame),
    ) -> Geometry {
        let mut last = self.key.borrow_mut();
        if last.as_ref() != Some(&key) {
            self.cache.clear();
            *last = Some(key);
        }
        self.cache.draw(renderer, size, draw)
    }
}

/// A color as a comparable key.
pub fn color_key(c: iced::Color) -> [u32; 4] {
    [c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()]
}
