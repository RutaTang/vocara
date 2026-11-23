//! The STFT spectrogram: its column transform, its assembly, and the batch
//! and streaming builders.

use vocara_model::models::spectrogram::Spectrogram;

use super::fft::{hanning, RealFft};
use super::rolling::RollingF32;
use super::{
    covered_max_hz, db20, pad_slice_into, spectrogram_bins, stft_frame_count, AnalysisParams,
    EMIT_BLOCK,
};

/// The dB image of a spectrogram as it is built: each column's [bins] values
/// are written STRAIGHT into one flat buffer, sized up front from the frame
/// count when the caller knows it. Every column used to be a list of its own
/// until the end, which then copied them all into the flat image, so a build
/// held its whole result twice (the 18 kHz EQ display of a 10-minute take:
/// ~173 MB of columns beside ~173 MB of image). The one assembly every
/// builder shares, so they agree on the dB range and the empty-input
/// defaults.
#[derive(Debug)]
pub(crate) struct SpectrogramImage {
    bins: usize,
    db: Vec<f32>,
    frames: usize,
}

impl SpectrogramImage {
    pub(crate) fn new(bins: usize, expected_frames: usize) -> Self {
        SpectrogramImage {
            bins,
            db: Vec::with_capacity(expected_frames * bins),
            frames: 0,
        }
    }

    /// Claims the next column; the caller writes its [bins] values there. An
    /// unforeseen extra column (a caller with no frame count) regrows the
    /// buffer.
    pub(crate) fn next_column(&mut self) -> &mut [f32] {
        let at = self.frames * self.bins;
        self.db.resize(at + self.bins, 0.0);
        self.frames += 1;
        &mut self.db[at..]
    }

    /// The finished [`Spectrogram`]: the buffer itself when the frame count
    /// was right, an exact-size copy only when fewer frames came (a file cut
    /// short on disk) or more (the buffer regrew), so the image never carries
    /// dead capacity into the caches that budget it by size.
    pub(crate) fn finish(
        self,
        max_hz: f64,
        sample_rate: i64,
        fft_size: usize,
        hop: usize,
    ) -> Spectrogram {
        let mut db = self.db;
        if db.capacity() != db.len() {
            db = db.into_boxed_slice().into_vec();
        }
        let mut min_db = f64::INFINITY;
        let mut max_db = f64::NEG_INFINITY;
        for &v in &db {
            let v = f64::from(v);
            if v < min_db {
                min_db = v;
            }
            if v > max_db {
                max_db = v;
            }
        }
        if self.frames == 0 {
            min_db = -90.0;
            max_db = 0.0;
        }
        Spectrogram {
            frames: self.frames,
            bins: self.bins,
            max_hz: covered_max_hz(max_hz, self.bins, sample_rate, fft_size),
            sample_rate,
            fft_size: fft_size as i64,
            hop: hop as i64,
            db,
            min_db,
            max_db,
        }
    }
}

/// The STFT behind the spectrogram-only paths and `StreamingStft`, one dB
/// column per frame: [fft_size] samples from frame·hop (0 past the end) times
/// the Hann window, transformed, 20·log10(|X| + 1e-9) for the first [bins]
/// bins. This is fftea's STFT (`run`, and `stream` + `flush` alike) exactly
/// — the same frames, the same window products, the same transform
/// ([`RealFft`]) and conversion — and it is the metrics pass's first FFT too
/// (both pinned by test).
pub(crate) struct ColumnStft {
    bins: usize,
    window: Vec<f64>,
    frame: Vec<f64>,
    mags: Vec<f64>,
    fft: RealFft,
}

impl ColumnStft {
    pub(crate) fn new(fft_size: usize, bins: usize) -> Self {
        ColumnStft {
            bins,
            window: hanning(fft_size),
            frame: vec![0.0; fft_size],
            mags: vec![0.0; bins],
            fft: RealFft::new(fft_size),
        }
    }

    /// The column of [samples] (one frame, already zero-padded) into [out]'s
    /// first [bins] values.
    pub(crate) fn column_into(&mut self, samples: &[f32], out: &mut [f32]) {
        for ((f, &s), &w) in self.frame.iter_mut().zip(samples).zip(&self.window) {
            *f = f64::from(s) * w;
        }
        self.fft.transform(&self.frame);
        self.fft.magnitudes_into(&mut self.mags, self.bins);
        for (o, &m) in out[..self.bins].iter_mut().zip(&self.mags) {
            *o = db20(m) as f32;
        }
    }
}

/// Incremental spectrogram builder with EXACTLY [`compute_spectrogram`]'s
/// columns (the same frames, zero-padded tail included, the same column
/// transform, the same assembly), fed in chunks so the full-track sample
/// buffer never has to exist: a frame is emitted once its samples are all
/// in, and what no later frame reads is dropped. Pinned equal to the batch
/// path by test.
pub struct StreamingSpectrogramBuilder {
    sample_rate: i64,
    fft_size: usize,
    hop: usize,
    max_hz: f64,
    stft: ColumnStft,
    image: SpectrogramImage,
    raw: RollingF32,
    window: Vec<f32>,
    next_frame: usize,
}

impl StreamingSpectrogramBuilder {
    /// [expected_frames] — the [`stft_frame_count`] of the whole input, when
    /// known (0 otherwise) — sizes the image once; without it the image
    /// grows as columns arrive.
    pub fn new(sample_rate: i64, params: AnalysisParams, expected_frames: usize) -> Self {
        let AnalysisParams {
            fft_size,
            hop,
            max_hz,
        } = params;
        let max_bin = spectrogram_bins(max_hz, fft_size, sample_rate);
        StreamingSpectrogramBuilder {
            sample_rate,
            fft_size,
            hop,
            max_hz,
            stft: ColumnStft::new(fft_size, max_bin),
            image: SpectrogramImage::new(max_bin, expected_frames),
            raw: RollingF32::default(),
            window: vec![0.0; fft_size],
            next_frame: 0,
        }
    }

    /// How many bins (DC up) each column keeps.
    pub fn max_bin(&self) -> usize {
        self.image.bins
    }

    /// Feeds the next chunk of mono samples, a block at a time so the buffer
    /// never holds (and compacts) a whole file-reader chunk.
    pub fn add(&mut self, chunk: &[f32]) {
        for block in chunk.chunks(EMIT_BLOCK) {
            self.raw.add_all(block);
            self.emit_ready(false);
        }
    }

    fn emit_ready(&mut self, flush: bool) {
        let len = self.raw.len();
        let n_frames = stft_frame_count(len, self.fft_size, self.hop);
        while self.next_frame < n_frames {
            let start = self.next_frame * self.hop;
            // Until the end is known, only frames whose samples are all in.
            if !flush && start + self.fft_size > len {
                return;
            }
            self.raw
                .slice_into(&mut self.window, start as i64, len as i64);
            self.stft
                .column_into(&self.window, self.image.next_column());
            self.next_frame += 1;
            self.raw.drop_below((self.next_frame * self.hop) as i64);
        }
    }

    /// Emits the trailing zero-padded frame and assembles the
    /// [`Spectrogram`]. Matches [`compute_spectrogram`] on empty input too:
    /// zero frames, never a lone zero-padded one.
    pub fn finish(mut self) -> Spectrogram {
        self.emit_ready(true);
        self.image
            .finish(self.max_hz, self.sample_rate, self.fft_size, self.hop)
    }
}

/// Computes a [`Spectrogram`] from mono [samples]: a dB column per STFT
/// frame ([`stft_frame_count`] framing, the last frame zero-padded), bins
/// from DC up to [`AnalysisParams::max_hz`]. Pure.
pub fn compute_spectrogram(
    samples: &[f32],
    sample_rate: i64,
    params: AnalysisParams,
) -> Spectrogram {
    let AnalysisParams {
        fft_size,
        hop,
        max_hz,
    } = params;
    let max_bin = spectrogram_bins(max_hz, fft_size, sample_rate);
    // Empty input → no frames, so VoiceMetrics.frames == Spectrogram.frames
    // stays aligned (never a lone zero-padded frame; see stft_frame_count).
    let len = samples.len();
    let n_frames = stft_frame_count(len, fft_size, hop);
    let mut image = SpectrogramImage::new(max_bin, n_frames);
    let mut stft = ColumnStft::new(fft_size, max_bin);
    let mut window = vec![0.0f32; fft_size];
    for fr in 0..n_frames {
        pad_slice_into(&mut window, samples, (fr * hop) as i64, len);
        stft.column_into(&window, image.next_column());
    }
    image.finish(max_hz, sample_rate, fft_size, hop)
}
