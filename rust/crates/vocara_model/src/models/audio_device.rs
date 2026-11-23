//! A selectable audio device (port of `lib/domain/models/audio_device.dart`).

/// A selectable audio device (currently output; input lands in a later phase).
/// [uid] is the stable CoreAudio device UID — the persist key — not the display name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioDevice {
    pub uid: String,
    pub name: String,
    /// Native sample rate in Hz (0 if unknown).
    pub rate: i64,
    pub is_default: bool,
    pub is_input: bool,
    /// On a Bluetooth transport. Selecting the same BT device for both input and output forces
    /// the headset into low-quality HFP mode (a hardware limit) — the UI warns about this.
    pub is_bluetooth: bool,
    /// How the device is attached. Decides which outputs the user may mark as headphones-only
    /// (see [`AudioTransport::can_carry_headphones_mark`]).
    pub transport: AudioTransport,
}

impl AudioDevice {
    /// `AudioDevice(uid:, name:)` with the defaults (rate 0, not default,
    /// output, not Bluetooth, unknown transport).
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> AudioDevice {
        AudioDevice {
            uid: uid.into(),
            name: name.into(),
            rate: 0,
            is_default: false,
            is_input: false,
            is_bluetooth: false,
            transport: AudioTransport::Unknown,
        }
    }
}

/// How a device is attached — the engine's `device::Transport`, in the same order (the index
/// crosses the FFI as `VeDeviceInfo.transport`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioTransport {
    Unknown,
    BuiltIn,
    Usb,
    Bluetooth,
    Hdmi,
    DisplayPort,
    Network,
    Thunderbolt,
    FireWire,
    Pci,
    Aggregate,
    Virtual,
    Other,
}

impl AudioTransport {
    /// `AudioTransport.values`, in the engine's order.
    pub const VALUES: [AudioTransport; 13] = [
        AudioTransport::Unknown,
        AudioTransport::BuiltIn,
        AudioTransport::Usb,
        AudioTransport::Bluetooth,
        AudioTransport::Hdmi,
        AudioTransport::DisplayPort,
        AudioTransport::Network,
        AudioTransport::Thunderbolt,
        AudioTransport::FireWire,
        AudioTransport::Pci,
        AudioTransport::Aggregate,
        AudioTransport::Virtual,
        AudioTransport::Other,
    ];

    /// The engine's code, [`AudioTransport::Unknown`] for one this build does not know.
    pub fn from_code(code: i64) -> AudioTransport {
        if code >= 0 && (code as usize) < AudioTransport::VALUES.len() {
            AudioTransport::VALUES[code as usize]
        } else {
            AudioTransport::Unknown
        }
    }

    /// The Dart enum name.
    pub fn name(self) -> &'static str {
        match self {
            AudioTransport::Unknown => "unknown",
            AudioTransport::BuiltIn => "builtIn",
            AudioTransport::Usb => "usb",
            AudioTransport::Bluetooth => "bluetooth",
            AudioTransport::Hdmi => "hdmi",
            AudioTransport::DisplayPort => "displayPort",
            AudioTransport::Network => "network",
            AudioTransport::Thunderbolt => "thunderbolt",
            AudioTransport::FireWire => "fireWire",
            AudioTransport::Pci => "pci",
            AudioTransport::Aggregate => "aggregate",
            AudioTransport::Virtual => "virtual",
            AudioTransport::Other => "other",
        }
    }

    /// Whether a device on this transport can be a pair of headphones the user marks as such: a
    /// product of its own (USB, Bluetooth, an interface). Never a port or a sink with speakers of
    /// its own — built-in (the jack, and an Intel Mac's one device for jack AND speakers), HDMI,
    /// DisplayPort, AirPlay — nor an aggregate or virtual device, which could be anything.
    pub fn can_carry_headphones_mark(self) -> bool {
        matches!(
            self,
            AudioTransport::Usb
                | AudioTransport::Bluetooth
                | AudioTransport::Thunderbolt
                | AudioTransport::FireWire
                | AudioTransport::Pci
        )
    }
}
