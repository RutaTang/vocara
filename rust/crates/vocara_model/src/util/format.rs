//! Clock formatting (port of `lib/utils/format.dart`).

use std::time::Duration;

/// Formats a millisecond offset as `mm:ss` for transport/clock displays
/// (`formatClockMs`; negative offsets read as 0, minutes are not capped).
pub fn format_clock_ms(milliseconds: i64) -> String {
    let ms = milliseconds.max(0);
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

/// Formats a duration as `mm:ss` (`formatClock`).
pub fn format_clock(duration: Duration) -> String {
    format_clock_ms(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_minutes_and_seconds_with_two_digits() {
        assert_eq!(format_clock_ms(0), "00:00");
        assert_eq!(format_clock_ms(-5000), "00:00");
        assert_eq!(format_clock_ms(61_999), "01:01");
        assert_eq!(format_clock_ms(6_000_000), "100:00");
        assert_eq!(format_clock(Duration::from_millis(59_500)), "00:59");
    }
}
