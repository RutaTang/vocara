//! The rolling sample buffer behind every streaming analyzer.

/// A growable append-only f32 buffer with COMPACTION: absolute indexing
/// survives dropping the consumed head, so the streaming analyzers' rolling
/// windows never hold more than the lookback they need.
#[derive(Debug)]
pub(crate) struct RollingF32 {
    /// The held samples, absolute indices `[base, len)`.
    buf: Vec<f32>,
    /// Absolute index of `buf[0]`.
    base: usize,
    /// Absolute total appended.
    len: usize,
}

/// The dead head a compaction waits for (a fresh buffer per compaction was a
/// 256 KB allocation every 32 frames of the raw signal alone).
const COMPACT_AT: usize = 1 << 14;

impl Default for RollingF32 {
    fn default() -> Self {
        RollingF32 {
            buf: Vec::with_capacity(1 << 15),
            base: 0,
            len: 0,
        }
    }
}

impl RollingF32 {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Empties the buffer, keeping its storage; absolute indices restart at 0.
    pub(crate) fn clear(&mut self) {
        self.buf.clear();
        self.base = 0;
        self.len = 0;
    }

    pub(crate) fn add(&mut self, v: f32) {
        self.buf.push(v);
        self.len += 1;
    }

    /// Appends all of [chunk].
    pub(crate) fn add_all(&mut self, chunk: &[f32]) {
        self.buf.extend_from_slice(chunk);
        self.len += chunk.len();
    }

    /// Drops everything below absolute index [keep_from] (clamped),
    /// compacting the storage when the dead head has grown past a block: the
    /// live tail slides to the front of the same buffer.
    pub(crate) fn drop_below(&mut self, keep_from: i64) {
        let target = keep_from.clamp(self.base as i64, self.len as i64) as usize;
        let dead = target - self.base;
        if dead < COMPACT_AT {
            return;
        }
        self.buf.drain(..dead);
        self.base = target;
    }

    /// Zero-padded window copy into [out] (the window is its length, the
    /// caller's buffer reused frame after frame); [max_len] caps the readable
    /// length (indices at or past it read 0 — the batch-equivalent signal
    /// length, see the decimator tail note in `StreamingTakeAnalyzer`).
    pub(crate) fn slice_into(&self, out: &mut [f32], start: i64, max_len: i64) {
        let count = out.len() as i64;
        // The readable span [from, end): held, and below max_len; the rest of
        // the window is zero.
        let base = self.base as i64;
        let from = start.max(base);
        let end = (start + count).min((self.len as i64).min(max_len));
        if end > from {
            let lead = (from - start) as usize;
            let tail = (end - start) as usize;
            out[..lead].fill(0.0);
            out[lead..tail]
                .copy_from_slice(&self.buf[(from - base) as usize..(end - base) as usize]);
            out[tail..].fill(0.0);
        } else {
            out.fill(0.0);
        }
    }
}
