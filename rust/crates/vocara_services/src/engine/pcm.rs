//! Decoded PCM handed back by the engine (the Dart `VePcm` struct, owned).

#[cfg(feature = "device")]
use std::ptr::NonNull;

#[cfg(feature = "device")]
use vocara_engine::ffi;

/// How a [`Pcm`]'s samples are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Interleaved stereo: `frames * 2` samples, L then R (`ve_decode`).
    InterleavedStereo,
    /// One sample per frame (`ve_decode_mono`, the L/R average).
    Mono,
}

impl Layout {
    /// Samples per frame.
    pub fn channels(self) -> usize {
        match self {
            Layout::InterleavedStereo => 2,
            Layout::Mono => 1,
        }
    }
}

/// Decoded samples in [-1, 1] (a decoder may overshoot; the engine does not
/// clamp), at [`Pcm::rate`].
///
/// A buffer the engine decoded is used where the engine allocated it, with no
/// copy, and freed exactly once when the `Pcm` drops, with the free call its
/// layout needs. The Dart side carried the raw address and frame count across
/// isolates and freed by hand on every path (a disposal mid-decode included);
/// ownership does that here. A `Pcm` can also hold samples built in Rust, so a
/// test fake can stand in for the engine's decode.
pub struct Pcm {
    buf: Buf,
    frames: usize,
    rate: u32,
    layout: Layout,
}

enum Buf {
    Owned(Vec<f32>),
    #[cfg(feature = "device")]
    Native(NativeBuf),
}

/// A buffer `ve_decode` or `ve_decode_mono` returned. Non-null; its length is
/// the owning [`Pcm`]'s `frames * layout.channels()`.
#[cfg(feature = "device")]
struct NativeBuf(NonNull<f32>);

// SAFETY: the buffer is a heap allocation nothing else refers to once the
// decode returned it (the engine keeps no handle), so moving its owner to
// another thread moves the only access to it; the engine documents it as
// freeable from any thread (the Dart side decoded in one isolate and freed in
// another).
#[cfg(feature = "device")]
unsafe impl Send for NativeBuf {}
// SAFETY: shared access only reads the samples (`Pcm::samples`); writing
// needs `&mut Pcm`.
#[cfg(feature = "device")]
unsafe impl Sync for NativeBuf {}

impl Pcm {
    /// Interleaved-stereo samples built in Rust. A stray trailing sample (an
    /// odd length) is not a frame and is dropped.
    pub fn interleaved_stereo(mut samples: Vec<f32>, rate: u32) -> Pcm {
        let frames = samples.len() / 2;
        samples.truncate(frames * 2);
        Pcm {
            buf: Buf::Owned(samples),
            frames,
            rate,
            layout: Layout::InterleavedStereo,
        }
    }

    /// Mono samples built in Rust.
    pub fn mono(samples: Vec<f32>, rate: u32) -> Pcm {
        Pcm {
            frames: samples.len(),
            buf: Buf::Owned(samples),
            rate,
            layout: Layout::Mono,
        }
    }

    /// Takes ownership of what a decode returned: `None` for the empty result
    /// (a null buffer, which needs no free).
    #[cfg(feature = "device")]
    pub(super) fn from_native(v: ffi::VePcm, layout: Layout) -> Option<Pcm> {
        let ptr = NonNull::new(v.ptr)?;
        Some(Pcm {
            buf: Buf::Native(NativeBuf(ptr)),
            frames: v.frames,
            rate: v.rate,
            layout,
        })
    }

    /// Frames (one sample per channel each).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The rate the samples are at: the rate the decode was asked for.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    /// The samples: `frames * layout.channels()` of them.
    pub fn samples(&self) -> &[f32] {
        match (&self.buf, self.frames * self.layout.channels()) {
            (Buf::Owned(v), _) => v,
            // SAFETY: the engine allocated exactly `frames * channels` f32s
            // here (aligned, initialized; a dangling but aligned pointer when
            // that is 0), and they stay valid until this `Pcm` frees them.
            #[cfg(feature = "device")]
            (Buf::Native(n), len) => unsafe { std::slice::from_raw_parts(n.0.as_ptr(), len) },
        }
    }

    /// The samples, writable (the analysis zeroes non-finite ones in place).
    pub fn samples_mut(&mut self) -> &mut [f32] {
        match (&mut self.buf, self.frames * self.layout.channels()) {
            (Buf::Owned(v), _) => v,
            // SAFETY: as in `samples`, and `&mut self` makes this the only
            // access to the buffer for the borrow's life.
            #[cfg(feature = "device")]
            (Buf::Native(n), len) => unsafe { std::slice::from_raw_parts_mut(n.0.as_ptr(), len) },
        }
    }

    /// The samples as a `Vec`: moved out when built in Rust; for an engine
    /// buffer, copied out before the buffer is freed (what Dart's
    /// `Float32List.fromList(ptr.asTypedList(n))` did).
    pub fn into_vec(mut self) -> Vec<f32> {
        let owned = match &mut self.buf {
            Buf::Owned(v) => Some(std::mem::take(v)),
            #[cfg(feature = "device")]
            Buf::Native(_) => None,
        };
        owned.unwrap_or_else(|| self.samples().to_vec())
    }
}

impl AsRef<[f32]> for Pcm {
    fn as_ref(&self) -> &[f32] {
        self.samples()
    }
}

impl std::fmt::Debug for Pcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pcm")
            .field("frames", &self.frames)
            .field("rate", &self.rate)
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "device")]
impl Drop for Pcm {
    fn drop(&mut self) {
        if let Buf::Native(n) = &self.buf {
            // SAFETY: `n` came from the decode matching `layout` and is freed
            // only here, once (drop runs once, and `into_vec` copies rather
            // than takes a native buffer); the free reconstructs the length
            // that decode allocated: `frames * 2` floats for stereo, `frames`
            // for mono.
            match self.layout {
                Layout::InterleavedStereo => ffi::ve_free_decoded(n.0.as_ptr(), self.frames),
                Layout::Mono => ffi::ve_free_decoded_mono(n.0.as_ptr(), self.frames),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_built_in_rust_counts_frames_and_drops_a_stray_sample() {
        let pcm = Pcm::interleaved_stereo(vec![0.1, 0.2, 0.3, 0.4, 0.5], 48_000);
        assert_eq!(pcm.frames(), 2);
        assert_eq!(pcm.rate(), 48_000);
        assert_eq!(pcm.layout(), Layout::InterleavedStereo);
        assert_eq!(pcm.samples(), &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(pcm.into_vec(), vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn mono_built_in_rust_is_writable_and_moves_out() {
        let mut pcm = Pcm::mono(vec![0.0, f32::NAN, 0.5], 16_000);
        assert_eq!((pcm.frames(), pcm.layout().channels()), (3, 1));
        for s in pcm.samples_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        assert_eq!(pcm.as_ref(), &[0.0, 0.0, 0.5]);
        assert_eq!(pcm.into_vec(), vec![0.0, 0.0, 0.5]);
        assert!(Pcm::mono(Vec::new(), 44_100).is_empty());
    }

    #[test]
    fn a_pcm_crosses_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Pcm>();
        let pcm = Pcm::mono(vec![1.0; 4], 8_000);
        let back = std::thread::spawn(move || pcm.frames()).join().unwrap();
        assert_eq!(back, 4);
    }
}
