//! The device IO buffer sizes a mic open changes, and how they are put back.
//!
//! Opening the VoiceProcessingIO unit asks the device(s) it binds for a small IO buffer (low
//! monitor latency, and an echo short enough for the canceller). That request used to outlive
//! the unit: after the first mic session, playback ran at the small buffer for the rest of the
//! process (more CPU and more underrun risk than the output was opened with), so the app
//! behaved differently after its first take. [`IoBufferRestore`] keeps what each device had
//! before, and puts it back when the unit closes, but only where the device still runs at what
//! the open set: a size someone else chose meanwhile is theirs to keep.
//!
//! Platform-free bookkeeping (the HAL reads and writes are the caller's), so it is tested
//! without hardware.

/// The original buffer sizes of the devices a mic open retuned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IoBufferRestore {
    /// `(device, original frames, frames set)`, one entry per device.
    changed: Vec<(u32, u32, u32)>,
}

impl IoBufferRestore {
    pub fn new() -> IoBufferRestore {
        IoBufferRestore::default()
    }

    /// Record that `device`, at `original` frames, was asked for `set` frames. A device seen
    /// before (one device serving both legs) keeps its first original: the second leg read the
    /// size the first leg had just set. Asking for what it already had changes nothing.
    pub fn record(&mut self, device: u32, original: u32, set: u32) {
        if self.changed.iter().any(|(d, _, _)| *d == device) || original == set {
            return;
        }
        self.changed.push((device, original, set));
    }

    /// The `(device, frames)` writes that put the sizes back, given each device's size `now`
    /// (`None`: unreadable, e.g. the device is gone). A device no longer at the size the open
    /// set is left alone. Empties the record: a restore runs once.
    pub fn take_restores(&mut self, now: impl Fn(u32) -> Option<u32>) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.changed)
            .into_iter()
            .filter(|&(device, _, set)| now(device) == Some(set))
            .map(|(device, original, _)| (device, original))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_retuned_device_gets_its_own_original_back_once() {
        let mut r = IoBufferRestore::new();
        r.record(7, 512, 256); // a duplex device: the input leg...
        r.record(7, 256, 256); // ...and the output leg, which read the size just set
        r.record(9, 1024, 256); // the default output, for an input-only mic
        assert_eq!(r.take_restores(|_| Some(256)), [(7, 512), (9, 1024)]);
        assert!(r.is_empty());
        assert!(
            r.take_restores(|_| Some(256)).is_empty(),
            "a restore runs once"
        );
    }

    #[test]
    fn a_size_changed_by_someone_else_or_a_device_gone_is_left_alone() {
        let mut r = IoBufferRestore::new();
        r.record(7, 512, 256);
        r.record(9, 1024, 256);
        r.record(11, 480, 256);
        let now = |d: u32| match d {
            7 => Some(128), // another client asked for less meanwhile
            9 => None,      // unplugged
            _ => Some(256),
        };
        assert_eq!(r.take_restores(now), [(11, 480)]);
    }

    #[test]
    fn a_device_already_at_the_size_asked_has_nothing_to_restore() {
        let mut r = IoBufferRestore::new();
        r.record(7, 256, 256);
        assert!(r.is_empty());
    }
}
