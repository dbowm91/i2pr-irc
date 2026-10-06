//! The one canonical IRCv3 protocol timestamp: `YYYY-MM-DDThh:mm:ss.sssZ`.
//!
//! This is deliberately a hand-rolled fixed-width UTC calendar type rather than a
//! date/time dependency. Two properties force that choice:
//!
//! 1. the wire grammar is *not* an epoch integer, so anything that round-trips
//!    through `SystemTime`/a generic datetime type would have to be reformatted and
//!    re-validated on every hop; and
//! 2. a leap second (`ss == 60`) is legal on the wire but unrepresentable in most
//!    calendar libraries, which silently normalise it to `:59` or roll it into the
//!    following minute. The bouncer replays *upstream-supplied* timestamps, so
//!    silently rewriting one would make the bouncer's history disagree with the
//!    server's.
//!
//! Everything here is fixed-width and allocation-free on the parse path, which
//! matters because it sits on the upstream read path and on durable history
//! comparison.

use core::fmt;

/// Exact canonical width, including the trailing `Z`.
///
/// The grammar has no variable-length form, so a timestamp longer than this is
/// malformed rather than merely unexpected.
pub const TIMESTAMP_BYTES: usize = 24;

/// Smallest accepted year. `0000` is rejected; the IRCv3 grammar is four digits
/// and year zero has no meaning for a UTC civil calendar.
pub const MIN_YEAR: u16 = 1;

/// Largest accepted year, fixed by the four-digit field width.
pub const MAX_YEAR: u16 = 9999;

/// Why a timestamp could not be parsed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampError {
    /// Not exactly [`TIMESTAMP_BYTES`] long.
    WrongLength,
    /// A byte where a fixed `YYYY-MM-DDThh:mm:ss.sssZ` separator or digit was required.
    NotADigit,
    /// Calendar-invalid month, day, hour, minute, or second.
    Calendar,
    /// `ss == 60` outside the only position a UTC leap second can occupy.
    LeapSecondPosition,
    /// Outside [`MIN_YEAR`]..=[`MAX_YEAR`].
    YearRange,
}

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::WrongLength => "timestamp must be exactly 24 bytes",
            Self::NotADigit => "timestamp contains a non-digit where a digit is required",
            Self::Calendar => "timestamp is not a valid calendar instant",
            Self::LeapSecondPosition => "leap second must appear as 23:59:60",
            Self::YearRange => "timestamp year is out of range",
        };
        f.write_str(text)
    }
}

impl std::error::Error for TimestampError {}

/// A parsed, validated IRCv3 protocol timestamp.
///
/// Ordering is total and deterministic even across a leap second: `:59.999` sorts
/// before `:60.000`, which sorts before the following minute's `:00.000`. Two
/// timestamps never compare equal unless they are the same instant.
///
/// This type is *metadata*. It never determines durable history order — that stays
/// `HistoryEventId` — because an upstream can send a skewed or repeated timestamp
/// and local order must stay stable regardless.
#[derive(Clone, Copy, Debug)]
pub struct IrcTimestamp {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    /// `0..=60`. `60` denotes a leap second and is preserved verbatim.
    second: u8,
    /// `0..=999`, always three digits on the wire.
    millis: u16,
}

impl IrcTimestamp {
    /// Parse the canonical wire form.
    ///
    /// Rejects anything that is not exactly `YYYY-MM-DDThh:mm:ss.sssZ`, including
    /// the numeric-offset forms (`+01:00`) and truncated fractional precision that
    /// other timestamp formats allow. For *this* protocol surface those are
    /// malformed input, not something to normalise.
    pub fn parse(value: &[u8]) -> Result<Self, TimestampError> {
        if value.len() != TIMESTAMP_BYTES {
            return Err(TimestampError::WrongLength);
        }

        // Fixed offsets of each numeric run in `YYYY-MM-DDThh:mm:ss.sssZ`.
        let year = digits(&value[0..4]) as u16;
        let month = digits(&value[5..7]) as u8;
        let day = digits(&value[8..10]) as u8;
        let hour = digits(&value[11..13]) as u8;
        let minute = digits(&value[14..16]) as u8;
        let second = digits(&value[17..19]) as u8;
        let millis = digits(&value[20..23]) as u16;

        if !matches!(value[4], b'-')
            || !matches!(value[7], b'-')
            || !matches!(value[10], b'T')
            || !matches!(value[13], b':')
            || !matches!(value[16], b':')
            || !matches!(value[19], b'.')
            || value[23] != b'Z'
        {
            return Err(TimestampError::NotADigit);
        }

        if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
            return Err(TimestampError::YearRange);
        }
        if !(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
        {
            return Err(TimestampError::Calendar);
        }
        if second > 60 {
            return Err(TimestampError::Calendar);
        }
        // A UTC leap second can only be inserted at the very end of a UTC day, so
        // `:60` is only meaningful at 23:59:60. Requiring that rejects nonsense
        // such as `12:30:60` without needing a leap-second table.
        if second == 60 && !(hour == 23 && minute == 59) {
            return Err(TimestampError::LeapSecondPosition);
        }

        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millis,
        })
    }

    /// Parse a `&str`, for call sites that already hold UTF-8.
    pub fn parse_str(value: &str) -> Result<Self, TimestampError> {
        Self::parse(value.as_bytes())
    }

    /// Build a timestamp from a UTC wall-clock reading in milliseconds since the
    /// Unix epoch.
    ///
    /// Used to synthesize a *local* timestamp when replaying an event the upstream
    /// never stamped. Readings outside the four-digit year window this type spans
    /// yield `None` rather than being clamped: a wall clock that far wrong must not
    /// become a fabricated wire timestamp.
    pub fn from_unix_millis(millis_since_epoch: i64) -> Option<Self> {
        // Bounds are 0001-01-01T00:00:00.000Z and 9999-12-31T23:59:59.999Z. The
        // explicit guard keeps the civil conversion far away from i64 arithmetic
        // overflow; the year check below is the authoritative one.
        const MIN_UNIX_MILLIS: i64 = -62_135_596_800_000;
        const MAX_UNIX_MILLIS: i64 = 253_402_300_799_999;
        if !(MIN_UNIX_MILLIS..=MAX_UNIX_MILLIS).contains(&millis_since_epoch) {
            return None;
        }
        let days = millis_since_epoch.div_euclid(86_400_000);
        let remainder = millis_since_epoch.rem_euclid(86_400_000);
        let (year, month, day) = civil_from_days(days);
        // Checked before the cast: `as u16` would silently truncate a larger year.
        if !(i64::from(MIN_YEAR)..=i64::from(MAX_YEAR)).contains(&year) {
            return None;
        }
        Some(Self {
            year: year as u16,
            month,
            day,
            hour: (remainder / 3_600_000) as u8,
            minute: ((remainder / 60_000) % 60) as u8,
            second: ((remainder / 1000) % 60) as u8,
            millis: (remainder % 1000) as u16,
        })
    }

    /// Canonical `(second, is_leap_second, millis)` sort key.
    ///
    /// The leap flag is compared *before* the milliseconds so that a leap second
    /// sorts after every ordinary value in the same second: `:59.999` < `:60.000` <
    /// `:60.999` < next minute `:00.000`, while `:60.000` and `:60.999` remain
    /// distinguishable from each other.
    fn sort_key(&self) -> (i64, bool, u16) {
        let days = days_from_civil(self.year as i64, self.month as i64, self.day as i64);
        let minute_start = days * 86_400 + self.hour as i64 * 3_600 + self.minute as i64 * 60;
        if self.second == 60 {
            (minute_start + 59, true, self.millis)
        } else {
            (minute_start + i64::from(self.second), false, self.millis)
        }
    }

    /// Milliseconds since the Unix epoch, using the same mapping as [`Self::sort_key`].
    ///
    /// A leap second reports within its own minute. Round-tripping this value
    /// through [`Self::from_unix_millis`] yields an ordinary `:59.mmm`, not
    /// `:60.mmm`; that loss is deliberate and is why the canonical *string*, not this
    /// number, is what gets stored.
    pub fn unix_millis(&self) -> i64 {
        let (seconds, _, millis) = self.sort_key();
        seconds * 1_000 + i64::from(millis)
    }

    /// Whether this instant is a leap second.
    pub fn is_leap_second(&self) -> bool {
        self.second == 60
    }

    /// Encode into the canonical 24-byte wire form.
    pub fn encode(&self) -> [u8; TIMESTAMP_BYTES] {
        let mut out = [0u8; TIMESTAMP_BYTES];
        write_digits(&mut out[0..4], self.year as u32);
        out[4] = b'-';
        write_digits(&mut out[5..7], self.month as u32);
        out[7] = b'-';
        write_digits(&mut out[8..10], self.day as u32);
        out[10] = b'T';
        write_digits(&mut out[11..13], self.hour as u32);
        out[13] = b':';
        write_digits(&mut out[14..16], self.minute as u32);
        out[16] = b':';
        write_digits(&mut out[17..19], self.second as u32);
        out[19] = b'.';
        write_digits(&mut out[20..23], self.millis as u32);
        out[23] = b'Z';
        out
    }
}

impl fmt::Display for IrcTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            core::str::from_utf8(&self.encode()).expect("timestamp is ASCII by construction"),
        )
    }
}

impl PartialEq for IrcTimestamp {
    fn eq(&self, other: &Self) -> bool {
        self.sort_key() == other.sort_key()
    }
}

impl Eq for IrcTimestamp {}

impl PartialOrd for IrcTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IrcTimestamp {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

/// Parse a fixed-width ASCII digit run, rejecting anything non-numeric.
fn digits(run: &[u8]) -> u32 {
    let mut value = 0u32;
    for byte in run {
        // Saturating here would silently accept `'9'`-as-locale input; the explicit
        // range check is what actually rejects malformed bytes.
        let Some(digit) = byte.checked_sub(b'0') else {
            return u32::MAX;
        };
        if digit > 9 {
            return u32::MAX;
        }
        value = value * 10 + u32::from(digit);
    }
    value
}

fn write_digits(out: &mut [u8], value: u32) {
    let width = out.len();
    for (index, slot) in out.iter_mut().enumerate() {
        let divisor = 10u32.pow((width - 1 - index) as u32);
        *slot = b'0' + ((value / divisor) % 10) as u8;
    }
}

fn is_gregorian_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_gregorian_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian civil date.
///
/// Howard Hinnant's `days_from_civil`, which is exact for the whole range and needs
/// no lookup tables. The input is always pre-validated, so the `MAX` sentinels
/// produced by [`digits`] can never reach it.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_position = (month + 9) % 12;
    let day_of_year = (153 * month_position + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`], returning `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u8;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u8;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn ts(text: &str) -> IrcTimestamp {
        IrcTimestamp::parse_str(text).expect("fixture must parse")
    }

    #[test]
    fn parses_the_specification_example_exactly() {
        // Literal example from the server-time extension.
        let value = ts("2011-10-19T16:40:51.620Z");
        assert_eq!(value.to_string(), "2011-10-19T16:40:51.620Z");
        assert!(!value.is_leap_second());
    }

    #[test]
    fn preserves_a_leap_second_rather_than_normalising_it() {
        // Literal example from the server-time extension: June 2012 leap second.
        let value = ts("2012-06-30T23:59:60.419Z");
        assert!(value.is_leap_second());
        assert_eq!(value.to_string(), "2012-06-30T23:59:60.419Z");
        // The crucial property: it is not rewritten to :59 and not rolled forward.
        assert_ne!(value.to_string(), "2012-06-30T23:59:59.419Z");
        assert_ne!(value.to_string(), "2012-07-01T00:00:00.419Z");
    }

    #[test]
    fn leap_second_orders_between_fifty_nine_and_the_next_minute() {
        let before = ts("2012-06-30T23:59:59.999Z");
        let leap = ts("2012-06-30T23:59:60.000Z");
        let after = ts("2012-07-01T00:00:00.000Z");
        assert_eq!(before.cmp(&leap), Ordering::Less);
        assert_eq!(leap.cmp(&after), Ordering::Less);
        assert_eq!(after.cmp(&leap), Ordering::Greater);
    }

    #[test]
    fn leap_second_millis_are_distinguishable() {
        let early = ts("2012-06-30T23:59:60.000Z");
        let late = ts("2012-06-30T23:59:60.999Z");
        assert_eq!(early.cmp(&late), Ordering::Less);
        assert_ne!(early, late);
    }

    #[test]
    fn rejects_an_integer_epoch_time_tag() {
        // The pre-corrective wire behaviour accepted this. It is not server-time.
        assert!(IrcTimestamp::parse_str("1700000000").is_err());
    }

    #[test]
    fn rejects_numeric_utc_offsets() {
        assert_eq!(
            IrcTimestamp::parse_str("2019-01-04T14:33:26.123+01:00"),
            Err(TimestampError::WrongLength)
        );
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26.123-0500").is_err());
    }

    #[test]
    fn rejects_lowercase_t_and_missing_zulu() {
        assert_eq!(
            IrcTimestamp::parse_str("2019-01-04t14:33:26.123Z"),
            Err(TimestampError::NotADigit)
        );
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26.123").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26.123z").is_err());
    }

    #[test]
    fn rejects_malformed_fractional_precision() {
        // Two digits, four digits, and no fractional part are all malformed here.
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26.12Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26.1234Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-04T14:33:26Z").is_err());
    }

    #[test]
    fn rejects_invalid_calendar_fields() {
        assert!(IrcTimestamp::parse_str("2019-13-04T14:33:26.123Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-00-04T14:33:26.123Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-32T14:33:26.123Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-00T14:33:26.123Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-04T24:33:26.123Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-01-04T14:60:26.123Z").is_err());
    }

    #[test]
    fn validates_days_against_gregorian_leap_years() {
        assert!(IrcTimestamp::parse_str("2020-02-29T00:00:00.000Z").is_ok());
        assert!(IrcTimestamp::parse_str("2000-02-29T00:00:00.000Z").is_ok());
        // Divisible by 4 but not a Gregorian leap year.
        assert!(IrcTimestamp::parse_str("1900-02-29T00:00:00.000Z").is_err());
        assert!(IrcTimestamp::parse_str("2019-02-29T00:00:00.000Z").is_err());
    }

    #[test]
    fn rejects_a_leap_second_outside_end_of_day() {
        assert_eq!(
            IrcTimestamp::parse_str("2012-06-30T12:30:60.000Z"),
            Err(TimestampError::LeapSecondPosition)
        );
        assert_eq!(
            IrcTimestamp::parse_str("2012-06-30T23:58:60.000Z"),
            Err(TimestampError::LeapSecondPosition)
        );
    }

    #[test]
    fn rejects_year_zero_and_out_of_range_years() {
        assert_eq!(
            IrcTimestamp::parse_str("0000-01-01T00:00:00.000Z"),
            Err(TimestampError::YearRange)
        );
        assert!(IrcTimestamp::parse_str("0001-01-01T00:00:00.000Z").is_ok());
        assert!(IrcTimestamp::parse_str("9999-12-31T23:59:59.999Z").is_ok());
    }

    #[test]
    fn round_trips_every_encoded_field() {
        for text in [
            "1970-01-01T00:00:00.000Z",
            "2011-10-19T16:40:51.620Z",
            "2012-06-30T23:59:60.419Z",
            "2024-02-29T12:00:00.001Z",
            "9999-12-31T23:59:59.999Z",
        ] {
            let parsed = ts(text);
            assert_eq!(parsed.encode().len(), TIMESTAMP_BYTES);
            assert_eq!(IrcTimestamp::parse(&parsed.encode()).unwrap(), parsed);
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn synthesizes_from_a_unix_millisecond_wall_clock() {
        // The exact timestamp used in the chathistory specification examples.
        let value = IrcTimestamp::from_unix_millis(1_546_612_406_123).expect("representable");
        assert_eq!(value.to_string(), "2019-01-04T14:33:26.123Z");
        let other = IrcTimestamp::from_unix_millis(0).expect("representable");
        assert_eq!(other.to_string(), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn synthesized_timestamps_round_trip_through_unix_millis() {
        for millis in [
            0i64,
            1,
            999,
            1_000,
            1_546_612_406_123,
            1_709_164_799_999,
            -1,
            -86_400_000,
        ] {
            let value = IrcTimestamp::from_unix_millis(millis).expect("representable");
            assert_eq!(value.unix_millis(), millis, "millis={millis}");
        }
    }

    #[test]
    fn synthesizes_before_the_epoch() {
        let value = IrcTimestamp::from_unix_millis(-1_000).expect("representable");
        assert_eq!(value.to_string(), "1969-12-31T23:59:59.000Z");
    }

    #[test]
    fn rejects_wall_clocks_outside_the_representable_window() {
        assert!(IrcTimestamp::from_unix_millis(i64::MAX).is_none());
        assert!(IrcTimestamp::from_unix_millis(i64::MIN).is_none());
        // Beyond year 9999.
        assert!(IrcTimestamp::from_unix_millis(253_402_300_800_000).is_none());
    }

    #[test]
    fn orders_identical_millisecond_values_as_equal() {
        assert_eq!(
            ts("2019-01-04T14:33:26.123Z"),
            ts("2019-01-04T14:33:26.123Z")
        );
    }

    #[test]
    fn civil_day_conversion_agrees_across_the_epoch() {
        // Spot-check the civil conversion against known days to catch drift.
        for (days, expected) in [
            (0i64, "1970-01-01"),
            (1, "1970-01-02"),
            (59, "1970-03-01"),
            (-1, "1969-12-31"),
            (19_723, "2024-01-01"),
        ] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(
                format!("{year:04}-{month:02}-{day:02}"),
                expected,
                "days={days}"
            );
            assert_eq!(days_from_civil(year, month as i64, day as i64), days);
        }
    }
}
