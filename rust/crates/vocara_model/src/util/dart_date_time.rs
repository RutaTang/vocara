//! Dart's `DateTime`, as the Dart VM implements it.
//!
//! Every model timestamp (`createdAt`, `updatedAt`, `addedAt`) is stored as
//! the text `DateTime.toIso8601String` writes and read back with
//! `DateTime.parse`, and that text is hashed for sync. The Flutter app writes
//! LOCAL times (`DateTime.now()`), so the text has no zone suffix and its
//! meaning depends on the machine's time zone. This type reproduces the VM's
//! rules exactly so a value read from the library writes back byte for byte:
//! - a value is a microsecond instant plus an `is_utc` flag; equality needs
//!   both (Dart's `==`), ordering only the instant (Dart's `compareTo`);
//! - local wall time comes from the C library's `localtime_r` (`tm_gmtoff`),
//!   with the VM's mapping of years outside 1901..2038 onto an equivalent
//!   year, and local fields are turned back into an instant with the VM's
//!   `_toLocalTimeOffset` search (the earliest of two valid instants when the
//!   clock went back; the earlier zone's reading when it jumped forward);
//! - `parse` accepts exactly what the VM's regular expression accepts, and
//!   `to_iso8601_string` writes microseconds only when there are any.

use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util::dart_num::floored_division;
use crate::Error;

const MICROS_PER_MILLI: i64 = 1_000;
const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;
const MICROS_PER_DAY: i64 = 24 * MICROS_PER_HOUR;
const SECONDS_PER_DAY: i64 = 86_400;

/// The largest |millisecondsSinceEpoch| a Dart `DateTime` can hold.
pub const MAX_MILLISECONDS_SINCE_EPOCH: i64 = 8_640_000_000_000_000;

const DAYS_UNTIL_MONTH: [[i64; 12]; 2] = [
    [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334],
    [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335],
];

/// A Dart `DateTime`: an instant in microseconds since the epoch, and whether
/// it reads in UTC or in the local time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DateTime {
    value: i64,
    is_utc: bool,
}

/// The broken-down fields of a [`DateTime`] in its own zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateParts {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub millisecond: i64,
    pub microsecond: i64,
    /// ISO weekday: Monday 1 .. Sunday 7.
    pub weekday: i64,
}

impl DateTime {
    /// `DateTime.now()`: the current instant, read in local time.
    pub fn now() -> DateTime {
        DateTime {
            value: current_micros(),
            is_utc: false,
        }
    }

    /// `DateTime.timestamp()` / `DateTime.now().toUtc()`.
    pub fn now_utc() -> DateTime {
        DateTime {
            value: current_micros(),
            is_utc: true,
        }
    }

    /// `DateTime(year, month, …)`: local wall-clock fields (out-of-range
    /// fields roll over, as in Dart). `None` where Dart throws (past the
    /// representable range).
    #[allow(clippy::too_many_arguments)]
    pub fn local(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
        millisecond: i64,
        microsecond: i64,
    ) -> Option<DateTime> {
        broken_down_date_to_value(
            year,
            month,
            day,
            hour,
            minute,
            second,
            millisecond,
            microsecond,
            false,
        )
        .map(|value| DateTime {
            value,
            is_utc: false,
        })
    }

    /// `DateTime.utc(year, month, …)`.
    #[allow(clippy::too_many_arguments)]
    pub fn utc(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
        millisecond: i64,
        microsecond: i64,
    ) -> Option<DateTime> {
        broken_down_date_to_value(
            year,
            month,
            day,
            hour,
            minute,
            second,
            millisecond,
            microsecond,
            true,
        )
        .map(|value| DateTime {
            value,
            is_utc: true,
        })
    }

    /// `DateTime(year, month, day)` for literal dates (tests, defaults).
    ///
    /// # Panics
    /// Where Dart throws: a date past the representable range (±275 000
    /// years).
    pub fn local_ymd(year: i64, month: i64, day: i64) -> DateTime {
        match DateTime::local(year, month, day, 0, 0, 0, 0, 0) {
            Some(d) => d,
            None => panic!("DateTime({year}, {month}, {day}) is out of range"),
        }
    }

    /// `DateTime.utc(year, month, day)` for literal dates.
    ///
    /// # Panics
    /// Where Dart throws: a date past the representable range.
    pub fn utc_ymd(year: i64, month: i64, day: i64) -> DateTime {
        match DateTime::utc(year, month, day, 0, 0, 0, 0, 0) {
            Some(d) => d,
            None => panic!("DateTime.utc({year}, {month}, {day}) is out of range"),
        }
    }

    /// `DateTime.fromMicrosecondsSinceEpoch`; `None` where Dart throws.
    pub fn from_micros_since_epoch(micros: i64, is_utc: bool) -> Option<DateTime> {
        if micros.unsigned_abs() > (MAX_MILLISECONDS_SINCE_EPOCH * MICROS_PER_MILLI) as u64 {
            return None;
        }
        Some(DateTime {
            value: micros,
            is_utc,
        })
    }

    /// `DateTime.fromMillisecondsSinceEpoch`; `None` where Dart throws.
    pub fn from_millis_since_epoch(millis: i64, is_utc: bool) -> Option<DateTime> {
        if millis.unsigned_abs() > MAX_MILLISECONDS_SINCE_EPOCH as u64 {
            return None;
        }
        Some(DateTime {
            value: millis * MICROS_PER_MILLI,
            is_utc,
        })
    }

    pub fn micros_since_epoch(&self) -> i64 {
        self.value
    }

    /// Floored, as Dart's getter.
    pub fn millis_since_epoch(&self) -> i64 {
        floored_division(self.value, MICROS_PER_MILLI)
    }

    pub fn is_utc(&self) -> bool {
        self.is_utc
    }

    /// `toUtc()`: the same instant, read in UTC.
    pub fn to_utc(&self) -> DateTime {
        DateTime {
            value: self.value,
            is_utc: true,
        }
    }

    /// `toLocal()`: the same instant, read in local time.
    pub fn to_local(&self) -> DateTime {
        DateTime {
            value: self.value,
            is_utc: false,
        }
    }

    /// `add(Duration(microseconds: micros))`.
    pub fn add_micros(&self, micros: i64) -> DateTime {
        DateTime {
            value: self.value + micros,
            is_utc: self.is_utc,
        }
    }

    /// `add(Duration(milliseconds: millis))`.
    pub fn add_millis(&self, millis: i64) -> DateTime {
        self.add_micros(millis * MICROS_PER_MILLI)
    }

    /// `difference(other).inMicroseconds`.
    pub fn difference_micros(&self, other: &DateTime) -> i64 {
        self.value - other.value
    }

    /// `compareTo`: by instant only, whatever the zones.
    pub fn compare_to(&self, other: &DateTime) -> Ordering {
        self.value.cmp(&other.value)
    }

    pub fn is_before(&self, other: &DateTime) -> bool {
        self.value < other.value
    }

    pub fn is_after(&self, other: &DateTime) -> bool {
        self.value > other.value
    }

    pub fn is_at_same_moment_as(&self, other: &DateTime) -> bool {
        self.value == other.value
    }

    /// `timeZoneOffset.inSeconds`: 0 in UTC.
    pub fn time_zone_offset_seconds(&self) -> i64 {
        if self.is_utc {
            0
        } else {
            time_zone_offset_in_seconds(self.value)
        }
    }

    /// The fields in this value's zone.
    pub fn parts(&self) -> DateParts {
        compute_upper_part(self.local_date_in_utc_micros())
    }

    pub fn year(&self) -> i64 {
        self.parts().year
    }
    pub fn month(&self) -> i64 {
        self.parts().month
    }
    pub fn day(&self) -> i64 {
        self.parts().day
    }
    pub fn hour(&self) -> i64 {
        self.parts().hour
    }
    pub fn minute(&self) -> i64 {
        self.parts().minute
    }
    pub fn second(&self) -> i64 {
        self.parts().second
    }
    pub fn millisecond(&self) -> i64 {
        self.parts().millisecond
    }
    pub fn microsecond(&self) -> i64 {
        self.parts().microsecond
    }
    pub fn weekday(&self) -> i64 {
        self.parts().weekday
    }

    fn local_date_in_utc_micros(&self) -> i64 {
        if self.is_utc {
            self.value
        } else {
            self.value + time_zone_offset_in_seconds(self.value) * MICROS_PER_SECOND
        }
    }

    /// `toIso8601String()`: `yyyy-MM-ddTHH:mm:ss.mmm[uuu][Z]` (six-digit
    /// signed years outside 0..9999; microseconds only when non-zero; `Z`
    /// only in UTC).
    pub fn to_iso8601_string(&self) -> String {
        let p = self.parts();
        let y = if (-9999..=9999).contains(&p.year) {
            four_digits(p.year)
        } else {
            six_digits(p.year)
        };
        let us = if p.microsecond == 0 {
            String::new()
        } else {
            three_digits(p.microsecond)
        };
        format!(
            "{y}-{}-{}T{}:{}:{}.{}{us}{}",
            two_digits(p.month),
            two_digits(p.day),
            two_digits(p.hour),
            two_digits(p.minute),
            two_digits(p.second),
            three_digits(p.millisecond),
            if self.is_utc { "Z" } else { "" }
        )
    }

    /// `DateTime.parse`: a FormatException ("Invalid date format" or "Time
    /// out of range") becomes [`Error::Format`].
    pub fn parse(formatted: &str) -> crate::Result<DateTime> {
        let Some(m) = match_parse_format(formatted) else {
            return Err(Error::format("Invalid date format", formatted));
        };
        let int = |s: &str| s.parse::<i64>().ok();
        // The matched digit runs are at most 6 long, so these never fail.
        let (Some(years), Some(month), Some(day)) = (int(m.year), int(m.month), int(m.day)) else {
            return Err(Error::format("Invalid date format", formatted));
        };
        let or_zero = |s: Option<&str>| s.and_then(int).unwrap_or(0);
        let hour = or_zero(m.hour);
        let mut minute = or_zero(m.minute);
        let second = or_zero(m.second);
        // Only the first 6 fraction digits count (microsecond precision).
        let milli_and_micro = match m.fraction {
            None => 0,
            Some(frac) => {
                let digits = frac.as_bytes();
                let mut result = 0i64;
                for i in 0..6 {
                    result *= 10;
                    if i < digits.len() {
                        result += i64::from(digits[i] ^ 0x30);
                    }
                }
                result
            }
        };
        let millisecond = milli_and_micro / MICROS_PER_MILLI;
        let microsecond = milli_and_micro % MICROS_PER_MILLI;
        let mut is_utc = false;
        if m.has_zone {
            is_utc = true;
            if let Some(sign) = m.zone_sign {
                let sign = if sign == '-' { -1 } else { 1 };
                let hour_difference = m.zone_hour.and_then(int).unwrap_or(0);
                let minute_difference = or_zero(m.zone_minute) + 60 * hour_difference;
                minute -= sign * minute_difference;
            }
        }
        match broken_down_date_to_value(
            years,
            month,
            day,
            hour,
            minute,
            second,
            millisecond,
            microsecond,
            is_utc,
        ) {
            Some(value) => Ok(DateTime { value, is_utc }),
            None => Err(Error::format("Time out of range", formatted)),
        }
    }

    /// `DateTime.tryParse`.
    pub fn try_parse(formatted: &str) -> Option<DateTime> {
        DateTime::parse(formatted).ok()
    }
}

fn current_micros() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_micros() as i64,
        Err(e) => -(e.duration().as_micros() as i64),
    }
}

fn four_digits(n: i64) -> String {
    let abs = n.unsigned_abs();
    let sign = if n < 0 { "-" } else { "" };
    if abs >= 1000 {
        n.to_string()
    } else if abs >= 100 {
        format!("{sign}0{abs}")
    } else if abs >= 10 {
        format!("{sign}00{abs}")
    } else {
        format!("{sign}000{abs}")
    }
}

fn six_digits(n: i64) -> String {
    let abs = n.unsigned_abs();
    let sign = if n < 0 { "-" } else { "+" };
    if abs >= 100_000 {
        format!("{sign}{abs}")
    } else {
        format!("{sign}0{abs}")
    }
}

fn three_digits(n: i64) -> String {
    if n >= 100 {
        n.to_string()
    } else if n >= 10 {
        format!("0{n}")
    } else {
        format!("00{n}")
    }
}

fn two_digits(n: i64) -> String {
    if n >= 10 {
        n.to_string()
    } else {
        format!("0{n}")
    }
}

fn is_leap_year(y: i64) -> bool {
    y % 4 == 0 && (y % 16 == 0 || y % 100 != 0)
}

/// Days since 1970 for the start of [year].
fn day_from_year(year: i64) -> i64 {
    365i64
        .wrapping_mul(year.wrapping_sub(1970))
        .wrapping_add(floored_division(year.wrapping_sub(1969), 4))
        .wrapping_sub(floored_division(year.wrapping_sub(1901), 100))
        .wrapping_add(floored_division(year.wrapping_sub(1601), 400))
}

#[allow(clippy::too_many_arguments)]
fn broken_down_date_to_value(
    mut year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millisecond: i64,
    microsecond: i64,
    is_utc: bool,
) -> Option<i64> {
    // Zero-based month, with under- and overflow carried into the year.
    let mut month = month - 1;
    if month >= 12 {
        year += month / 12;
        month %= 12;
    } else if month < 0 {
        let real_month = month.rem_euclid(12);
        year += (month - real_month) / 12;
        month = real_month;
    }
    // Dart's 64-bit int arithmetic wraps; so does this (only an absurd
    // six-digit year can get there).
    let mut days = day.wrapping_sub(1);
    days = days.wrapping_add(DAYS_UNTIL_MONTH[usize::from(is_leap_year(year))][month as usize]);
    days = days.wrapping_add(day_from_year(year));
    let mut micros = days
        .wrapping_mul(MICROS_PER_DAY)
        .wrapping_add(hour.wrapping_mul(MICROS_PER_HOUR))
        .wrapping_add(minute.wrapping_mul(MICROS_PER_MINUTE))
        .wrapping_add(second.wrapping_mul(MICROS_PER_SECOND))
        .wrapping_add(millisecond.wrapping_mul(MICROS_PER_MILLI))
        .wrapping_add(microsecond);
    if !is_utc {
        // The zone offset is under a day, so anything further out than the
        // range plus a day can never become valid.
        if micros.wrapping_abs() > MAX_MILLISECONDS_SINCE_EPOCH * MICROS_PER_MILLI + MICROS_PER_DAY
        {
            return None;
        }
        micros -= to_local_time_offset(micros);
    }
    if micros.wrapping_abs() > MAX_MILLISECONDS_SINCE_EPOCH * MICROS_PER_MILLI {
        return None;
    }
    Some(micros)
}

/// The VM's breakdown of a (zone-adjusted) microsecond value into fields.
fn compute_upper_part(local_micros: i64) -> DateParts {
    const DAYS_IN_4_YEARS: i64 = 4 * 365 + 1;
    const DAYS_IN_100_YEARS: i64 = 25 * DAYS_IN_4_YEARS - 1;
    const DAYS_IN_400_YEARS: i64 = 4 * DAYS_IN_100_YEARS + 1;
    const DAYS_1970_TO_2000: i64 = 30 * 365 + 7;
    const DAYS_OFFSET: i64 = 1000 * DAYS_IN_400_YEARS + 5 * DAYS_IN_400_YEARS - DAYS_1970_TO_2000;
    const YEARS_OFFSET: i64 = 400_000;

    let days_since_1970 = floored_division(local_micros, MICROS_PER_DAY);
    let mut days = days_since_1970 + DAYS_OFFSET;
    let mut year = 400 * (days / DAYS_IN_400_YEARS) - YEARS_OFFSET;
    days %= DAYS_IN_400_YEARS;
    days -= 1;
    let yd1 = days / DAYS_IN_100_YEARS;
    days %= DAYS_IN_100_YEARS;
    year += 100 * yd1;
    days += 1;
    let yd2 = days / DAYS_IN_4_YEARS;
    days %= DAYS_IN_4_YEARS;
    year += 4 * yd2;
    days -= 1;
    let yd3 = days / 365;
    days %= 365;
    year += yd3;

    let is_leap = (yd1 == 0 || yd2 != 0) && yd3 == 0;
    if is_leap {
        days += 1;
    }
    let until = &DAYS_UNTIL_MONTH[usize::from(is_leap)];
    let mut month = 12usize;
    while until[month - 1] > days {
        month -= 1;
    }
    let day = days - until[month - 1] + 1;

    DateParts {
        year,
        month: month as i64,
        day,
        hour: floored_division(local_micros, MICROS_PER_HOUR).rem_euclid(24),
        minute: floored_division(local_micros, MICROS_PER_MINUTE).rem_euclid(60),
        second: floored_division(local_micros, MICROS_PER_SECOND).rem_euclid(60),
        millisecond: floored_division(local_micros, MICROS_PER_MILLI).rem_euclid(1000),
        microsecond: local_micros.rem_euclid(MICROS_PER_MILLI),
        // 1970-01-01 was a Thursday (4); Monday is 1.
        weekday: (days_since_1970 + 4 - 1).rem_euclid(7) + 1,
    }
}

fn week_day(year: i64) -> i64 {
    (day_from_year(year) + 4).rem_euclid(7)
}

/// A year in 2008..2035 with the same leap-ness and first weekday.
fn equivalent_year(year: i64) -> i64 {
    let recent_year = (if is_leap_year(year) { 1956 } else { 1967 }) + week_day(year) * 12;
    2008 + (recent_year - 2008).rem_euclid(28)
}

fn years_from_seconds_since_epoch(seconds: i64) -> i64 {
    const DAYS_IN_4_YEARS: i64 = 4 * 365 + 1;
    const DAYS_IN_100_YEARS: i64 = 25 * DAYS_IN_4_YEARS - 1;
    const DAYS_YEAR_2098: i64 = DAYS_IN_100_YEARS + 6 * DAYS_IN_4_YEARS;
    let days = seconds / SECONDS_PER_DAY;
    if days > 0 && days < DAYS_YEAR_2098 {
        return 1970 + (4 * days + 2) / DAYS_IN_4_YEARS;
    }
    compute_upper_part(seconds.saturating_mul(MICROS_PER_SECOND)).year
}

/// Seconds with the same fields but a year inside 1901..2038 (what the C
/// library's zone rules are asked about).
fn equivalent_seconds(micros: i64) -> i64 {
    const CUT_OFF_SECONDS: i64 = 0x7FFF_FFFF;
    let mut seconds = floored_division(micros, MICROS_PER_SECOND);
    if seconds.abs() > CUT_OFF_SECONDS {
        let year = years_from_seconds_since_epoch(seconds);
        let days = day_from_year(year);
        let equivalent_days = day_from_year(equivalent_year(year));
        seconds += (equivalent_days - days) * SECONDS_PER_DAY;
    }
    seconds
}

fn time_zone_offset_in_seconds(micros: i64) -> i64 {
    local_offset_for_clamped_seconds(equivalent_seconds(micros))
}

/// The local zone's offset east of UTC at [seconds], from `localtime_r`'s
/// `tm_gmtoff` (0 when it cannot say, as the VM does).
#[cfg(unix)]
fn local_offset_for_clamped_seconds(seconds: i64) -> i64 {
    // A `time_t` too narrow for [seconds] cannot say (as the VM checks).
    let t = seconds as libc::time_t;
    if t as i64 != seconds {
        return 0;
    }
    // SAFETY: `libc::tm` is plain old data (integers and one pointer), for
    // which all-zero bytes are a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` reads `t` and writes only into `tm`; both are
    // live, exclusively borrowed locals for the duration of the call.
    let result = unsafe { libc::localtime_r(&t, &mut tm) };
    if result.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn local_offset_for_clamped_seconds(_seconds: i64) -> i64 {
    0
}

/// The VM's search for the offset that turns local wall-clock fields
/// (encoded as if UTC in [micros]) into an instant.
fn to_local_time_offset(micros: i64) -> i64 {
    let offset_at = |m: i64| time_zone_offset_in_seconds(m) * MICROS_PER_SECOND;
    let mut offset = offset_at(micros);
    if offset != 0 {
        let offset2 = offset_at(micros - offset);
        if offset2 != offset {
            // A second zone within the day: try again in it. Either it is a
            // solution, or there is none and the lower offset (the later
            // instant) wins.
            let offset3 = offset_at(micros - offset2);
            return offset2.min(offset3);
        }
        offset = offset2;
    }
    // An earlier zone (changes are assumed to be at most two hours) may also
    // hold this wall time: the earliest solution wins.
    let offset4 = offset_at(micros - offset - 2 * MICROS_PER_HOUR);
    if offset4 > offset {
        if offset4 == offset + 2 * MICROS_PER_HOUR {
            return offset4;
        }
        let offset5 = offset_at(micros - offset4);
        if offset5 == offset4 {
            return offset4;
        }
    }
    offset
}

/// The groups of Dart's `DateTime.parse` regular expression:
/// `^([+-]?\d{4,6})-?(\d\d)-?(\d\d)(?:[ T](\d\d)(?::?(\d\d)(?::?(\d\d)
/// (?:[.,](\d+))?)?)?( ?[zZ]| ?([-+])(\d\d)(?::?(\d\d))?)?)?$`.
struct ParseMatch<'a> {
    year: &'a str,
    month: &'a str,
    day: &'a str,
    hour: Option<&'a str>,
    minute: Option<&'a str>,
    second: Option<&'a str>,
    fraction: Option<&'a str>,
    has_zone: bool,
    zone_sign: Option<char>,
    zone_hour: Option<&'a str>,
    zone_minute: Option<&'a str>,
}

/// A cursor over ASCII-pattern input (`\d` is ASCII only in Dart).
struct Cursor<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }
    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn digits(&mut self, n: usize) -> Option<&'a str> {
        let bytes = self.s.as_bytes();
        if self.i + n <= bytes.len() && bytes[self.i..self.i + n].iter().all(u8::is_ascii_digit) {
            let out = &self.s[self.i..self.i + n];
            self.i += n;
            Some(out)
        } else {
            None
        }
    }
    fn digit_run(&mut self) -> Option<&'a str> {
        let start = self.i;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.i += 1;
        }
        (self.i > start).then(|| &self.s[start..self.i])
    }
    fn at_end(&self) -> bool {
        self.i == self.s.len()
    }
}

fn match_parse_format(s: &str) -> Option<ParseMatch<'_>> {
    let bytes = s.as_bytes();
    let sign_len = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let run = bytes[sign_len..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    // `\d{4,6}` is greedy and backtracks: the longest year that lets the rest
    // match wins (the compact `20260101` needs the four-digit one). Past the
    // year, every optional part either matches greedily or the whole match
    // fails, so the rest needs no backtracking.
    for year_len in (4..=run.min(6)).rev() {
        if let Some(m) = match_after_year(s, sign_len + year_len) {
            return Some(m);
        }
    }
    None
}

fn match_after_year(s: &str, year_end: usize) -> Option<ParseMatch<'_>> {
    let mut c = Cursor { s, i: year_end };
    c.eat(b'-');
    let month = c.digits(2)?;
    c.eat(b'-');
    let day = c.digits(2)?;
    let mut m = ParseMatch {
        year: &s[..year_end],
        month,
        day,
        hour: None,
        minute: None,
        second: None,
        fraction: None,
        has_zone: false,
        zone_sign: None,
        zone_hour: None,
        zone_minute: None,
    };
    if c.at_end() {
        return Some(m);
    }
    if !(c.eat(b' ') || c.eat(b'T')) {
        return None;
    }
    m.hour = Some(c.digits(2)?);
    // (?::?(\d\d)(?::?(\d\d)(?:[.,](\d+))?)?)?
    let save = c.i;
    c.eat(b':');
    if let Some(minute) = c.digits(2) {
        m.minute = Some(minute);
        let save2 = c.i;
        c.eat(b':');
        if let Some(second) = c.digits(2) {
            m.second = Some(second);
            let save3 = c.i;
            if c.eat(b'.') || c.eat(b',') {
                match c.digit_run() {
                    Some(frac) => m.fraction = Some(frac),
                    None => c.i = save3,
                }
            }
        } else {
            c.i = save2;
        }
    } else {
        c.i = save;
    }
    // ( ?[zZ]| ?([-+])(\d\d)(?::?(\d\d))?)?
    let save = c.i;
    c.eat(b' ');
    if c.eat(b'z') || c.eat(b'Z') {
        m.has_zone = true;
    } else if let Some(sign @ (b'+' | b'-')) = c.peek() {
        c.i += 1;
        match c.digits(2) {
            Some(zone_hour) => {
                m.has_zone = true;
                m.zone_sign = Some(sign as char);
                m.zone_hour = Some(zone_hour);
                let save2 = c.i;
                c.eat(b':');
                match c.digits(2) {
                    Some(zone_minute) => m.zone_minute = Some(zone_minute),
                    None => c.i = save2,
                }
            }
            None => c.i = save,
        }
    } else {
        c.i = save;
    }
    c.at_end().then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_values_round_trip_through_the_iso_text() {
        for text in [
            "2026-01-01T00:00:00.000Z",
            "2026-06-18T12:34:56.789Z",
            "2026-06-18T12:34:56.789012Z",
            "1969-12-31T23:59:59.999Z",
            "0001-01-01T00:00:00.000Z",
            "+010000-01-01T00:00:00.000Z",
            "-0001-12-31T00:00:00.000Z",
        ] {
            assert_eq!(DateTime::parse(text).unwrap().to_iso8601_string(), text);
        }
    }

    #[test]
    fn parse_accepts_what_the_dart_regex_accepts() {
        let utc = |s: &str| DateTime::parse(s).unwrap().to_utc().to_iso8601_string();
        assert_eq!(utc("20260102T030405Z"), "2026-01-02T03:04:05.000Z");
        assert_eq!(utc("2026-01-02 03:04:05,5z"), "2026-01-02T03:04:05.500Z");
        assert_eq!(
            utc("2026-01-02T03:04:05.1234567Z"),
            "2026-01-02T03:04:05.123456Z"
        );
        assert_eq!(utc("2026-01-02T03:04+0130"), "2026-01-02T01:34:00.000Z");
        assert_eq!(utc("2026-01-02T03 -01:00"), "2026-01-02T04:00:00.000Z");
        assert_eq!(utc("2026-13-01T00:00:00Z"), "2027-01-01T00:00:00.000Z");
        assert!(DateTime::parse("2026-01-02T03:04:05").is_ok_and(|d| !d.is_utc()));
        for bad in [
            "2026-1-01",
            "2026-01-01T",
            "2026-01-01Z",
            "26-01-01",
            "2026-01-01T03:4",
            "x",
        ] {
            assert!(DateTime::parse(bad).is_err(), "{bad}");
        }
        assert!(matches!(
            DateTime::parse("+275761-01-01T00:00:00Z"),
            Err(Error::Format { ref message, .. }) if message == "Time out of range"
        ));
    }

    #[test]
    fn equality_needs_the_zone_and_ordering_does_not() {
        let a = DateTime::utc_ymd(2026, 1, 1);
        let b = a.to_local();
        assert_ne!(a, b);
        assert_eq!(a.compare_to(&b), Ordering::Equal);
        assert!(a.is_at_same_moment_as(&b));
    }

    #[test]
    fn local_values_round_trip_in_any_zone() {
        let d = DateTime::local_ymd(2026, 6, 18);
        let text = d.to_iso8601_string();
        assert_eq!(text, "2026-06-18T00:00:00.000");
        assert_eq!(DateTime::parse(&text).unwrap(), d);
        assert_eq!(d.weekday(), 4);
    }
}
