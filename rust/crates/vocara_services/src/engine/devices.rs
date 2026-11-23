//! Audio device enumeration and the input channel count: host queries that
//! need no running engine, so the settings panel can list devices with no
//! song open.

use std::ffi::c_char;

use vocara_model::models::audio_device::{AudioDevice, AudioTransport};

use super::c_chars_to_string;
#[cfg(feature = "device")]
use super::c_string;
#[cfg(feature = "device")]
use vocara_engine::ffi;

/// Most devices read in one enumeration (generous; real systems have fewer
/// than 16). A system with more lists the first this many.
pub const DEVICE_ENUM_CAP: usize = 32;

/// Width of every device uid read from the engine: `VeDeviceInfo.uid` and the
/// uid readers' buffers alike, so a listed uid and a reported one truncate the
/// same way (at half the width, a long uid was never found in the lists).
pub const UID_CAP: usize = 256;

/// The output devices (`ve_enum_output_devices`); empty without an engine.
pub fn enumerate_output_devices() -> Vec<AudioDevice> {
    enumerate(false)
}

/// The input devices (`ve_enum_input_devices`); empty without an engine.
pub fn enumerate_input_devices() -> Vec<AudioDevice> {
    enumerate(true)
}

/// The input channel count of the device with `uid` ("" = the system default
/// input), from its current stream format (`ve_input_device_channels`); 0 when
/// the device is absent or unreadable, or there is no engine. The settings
/// panel offers the input-channel choice only for a device with 2+ inputs.
pub fn input_device_channel_count(uid: &str) -> u32 {
    #[cfg(feature = "device")]
    {
        let cuid = c_string(uid);
        // SAFETY: `cuid` is a NUL-terminated string alive for the call.
        ffi::ve_input_device_channels(cuid.as_ptr())
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = uid;
        0
    }
}

fn enumerate(input: bool) -> Vec<AudioDevice> {
    #[cfg(feature = "device")]
    {
        let mut buf: Vec<ffi::VeDeviceInfo> = (0..DEVICE_ENUM_CAP)
            // SAFETY: VeDeviceInfo is plain old data (char arrays and
            // integers), for which all-zero bytes are a valid value.
            .map(|_| unsafe { std::mem::zeroed() })
            .collect();
        // SAFETY: `buf` holds DEVICE_ENUM_CAP entries, the cap passed; the
        // engine writes at most that many and returns the total it found.
        let total = if input {
            ffi::ve_enum_input_devices(buf.as_mut_ptr(), DEVICE_ENUM_CAP)
        } else {
            ffi::ve_enum_output_devices(buf.as_mut_ptr(), DEVICE_ENUM_CAP)
        };
        buf.truncate(total.min(DEVICE_ENUM_CAP));
        buf.iter()
            .map(|d| {
                device(
                    &d.uid,
                    &d.name,
                    d.rate,
                    d.is_default,
                    d.is_bluetooth,
                    d.transport,
                    input,
                )
            })
            .collect()
    }
    #[cfg(not(feature = "device"))]
    {
        let _ = input;
        Vec::new()
    }
}

/// One `VeDeviceInfo`'s fields as the domain's device. `input` is the list it
/// came from (the Dart side set `isInput` from the call, not the struct).
#[cfg_attr(not(feature = "device"), allow(dead_code))]
fn device(
    uid: &[c_char],
    name: &[c_char],
    rate: u32,
    is_default: u8,
    is_bluetooth: u8,
    transport: u8,
    input: bool,
) -> AudioDevice {
    AudioDevice {
        uid: c_chars_to_string(uid),
        name: c_chars_to_string(name),
        rate: i64::from(rate),
        is_default: is_default != 0,
        is_input: input,
        is_bluetooth: is_bluetooth != 0,
        transport: AudioTransport::from_code(i64::from(transport)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero-padded C char field of `width`, as the engine fills it.
    fn field(s: &str, width: usize) -> Vec<c_char> {
        let mut v: Vec<c_char> = s
            .bytes()
            .map(|b| c_char::from_ne_bytes(b.to_ne_bytes()))
            .collect();
        v.resize(width, 0);
        v
    }

    #[test]
    fn an_entry_becomes_the_domain_device() {
        let d = device(
            &field("AppleUSBAudioEngine:1", UID_CAP),
            &field("Scarlett 2i2", 128),
            48_000,
            1,
            0,
            2,
            true,
        );
        assert_eq!(d.uid, "AppleUSBAudioEngine:1");
        assert_eq!(d.name, "Scarlett 2i2");
        assert_eq!(d.rate, 48_000);
        assert!(d.is_default && d.is_input && !d.is_bluetooth);
        assert_eq!(d.transport, AudioTransport::Usb);
        // A transport code this build does not know reads as unknown.
        let bt = device(&field("bt", 8), &field("AirPods", 8), 0, 0, 1, 99, false);
        assert!(bt.is_bluetooth && !bt.is_input && !bt.is_default);
        assert_eq!(bt.transport, AudioTransport::Unknown);
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_the_device_feature_there_are_no_devices() {
        assert!(enumerate_output_devices().is_empty());
        assert!(enumerate_input_devices().is_empty());
        assert_eq!(input_device_channel_count(""), 0);
    }
}
