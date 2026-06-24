//! Flush-to-zero control for the audio thread.
//!
//! Denormal (subnormal) floats cause 10–100× slowdowns on the comb/allpass feedback paths
//! of the reverb as a tail decays toward silence. There is no portable std API, and
//! `_mm_setcsr` is deprecated; critically, **aarch64 `FPCR` is per-thread and is NOT
//! inherited by the OS-created audio thread**, so FTZ must be set on the callback's own
//! thread (cheaply, once per callback via [`FlushGuard`]).

/// RAII guard: sets flush-to-zero on construction, restores the previous mode on drop.
/// Construct it first thing in the audio callback.
pub struct FlushGuard {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    prev: u64,
}

impl FlushGuard {
    // Each arch branch returns early; the `return`s are a cfg artifact, not redundant.
    #[allow(clippy::needless_return)]
    #[inline]
    pub fn new() -> Self {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let mut fpcr: u64;
            core::arch::asm!("mrs {0}, fpcr", out(reg) fpcr, options(nomem, nostack));
            let prev = fpcr;
            fpcr |= 1 << 24; // FZ: flush-to-zero
            core::arch::asm!("msr fpcr, {0}", in(reg) fpcr, options(nomem, nostack));
            return FlushGuard { prev };
        }
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let mut mxcsr: u32 = 0;
            core::arch::asm!("stmxcsr [{0}]", in(reg) &mut mxcsr, options(nostack));
            let prev = mxcsr as u64;
            mxcsr |= (1 << 15) | (1 << 6); // FTZ | DAZ
            core::arch::asm!("ldmxcsr [{0}]", in(reg) &mxcsr, options(nostack));
            return FlushGuard { prev };
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        FlushGuard {}
    }
}

impl Default for FlushGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FlushGuard {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr fpcr, {0}", in(reg) self.prev, options(nomem, nostack));
        }
        #[cfg(target_arch = "x86_64")]
        unsafe {
            let v = self.prev as u32;
            core::arch::asm!("ldmxcsr [{0}]", in(reg) &v, options(nostack));
        }
    }
}

/// Belt-and-suspenders: a tiny bias added into a feedback accumulator so a value decaying
/// toward silence can never enter subnormal arithmetic, even if FTZ is somehow not honored
/// (e.g. a future auto-vectorized path the compiler assumes runs in default FP mode).
pub const ANTI_DENORMAL: f32 = 1.0e-18;

/// Replace a non-finite (NaN/inf) value with 0 so one bad sample can't poison recursive
/// state for the rest of the session.
#[inline]
pub fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}
