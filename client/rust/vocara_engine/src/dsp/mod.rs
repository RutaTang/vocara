//! DSP node kernels: EQ, compressor, reverb, gain/pan — plus denormal control + smoothing.

pub mod biquad;
pub mod capture_cleanup;
pub mod compressor;
pub mod deesser;
pub mod delay;
pub mod denormals;
pub mod doubler;
pub mod gain_pan;
pub mod gate;
pub mod limiter;
pub mod master;
pub mod reverb;
pub mod saturation;
pub mod smooth;
