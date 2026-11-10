//! FAT timestamps: DOS date and time (2-second steps, local time, 1980..2107) to and from Unix time.
//!
//! The volume's `time_offset` (seconds east of UTC, 0 by default, like Linux without `tz=`/`time_offset=`) turns the
//! stored local time into UTC.

/// A point in time: seconds and nanoseconds since 1970-01-01 00:00:00 UTC.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Time {
    pub secs: i64,
    pub nsec: u32,
}

impl Time {
    pub const fn new(secs: i64, nsec: u32) -> Self {
        Time { secs, nsec }
    }
}

/// 1980-01-01 00:00:00 UTC, the earliest FAT timestamp.
const FAT_EPOCH: i64 = 315_532_800;
/// 2107-12-31 23:59:58 UTC, the latest.
const FAT_LAST: i64 = 4_354_819_198;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Decodes a DOS date, time and (for creation times) hundredths of a second into a [`Time`].
///
/// An impossible date (month 0 or above 12, day 0) reads as 1980-01-01, like Linux.
pub fn from_dos(date: u16, time: u16, centis: u8, offset_secs: i64) -> Time {
    let year = 1980 + i64::from(date >> 9);
    let month = i64::from((date >> 5) & 0xF);
    let day = i64::from(date & 0x1F);
    let (month, day) = if (1..=12).contains(&month) && day >= 1 {
        (month, day)
    } else {
        (1, 1)
    };
    let hour = i64::from(time >> 11).min(23);
    let min = i64::from((time >> 5) & 0x3F).min(59);
    let sec = i64::from((time & 0x1F) * 2).min(59);
    let mut secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec;
    secs -= offset_secs;
    let centis = u32::from(centis.min(199));
    Time {
        secs: secs + i64::from(centis / 100),
        nsec: (centis % 100) * 10_000_000,
    }
}

/// Encodes a [`Time`] as (date, time, centiseconds 0..=199). Times outside 1980..2107 are clamped.
pub fn to_dos(t: Time, offset_secs: i64) -> (u16, u16, u8) {
    let local = (t.secs + offset_secs).clamp(FAT_EPOCH, FAT_LAST);
    let days = local.div_euclid(86_400);
    let rem = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = (((rem / 3600) as u16) << 11)
        | ((((rem % 3600) / 60) as u16) << 5)
        | ((rem % 60 / 2) as u16);
    let centis = ((rem % 2) as u32 * 100 + t.nsec / 10_000_000).min(199) as u8;
    (date, time, centis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_round_trip() {
        for secs in [
            FAT_EPOCH,
            FAT_EPOCH + 1,
            951_782_400, /* 2000-02-29 */
            1_793_534_400,
            FAT_LAST,
        ] {
            let t = Time::new(secs, 0);
            let (d, tm, c) = to_dos(t, 0);
            let back = from_dos(d, tm, c, 0);
            assert_eq!(back.secs, secs, "{secs}");
        }
        // 2-second steps: an odd second reads back one lower unless the centiseconds carry it.
        let (d, tm, c) = to_dos(Time::new(FAT_EPOCH + 3, 250_000_000), 0);
        let back = from_dos(d, tm, c, 0);
        assert_eq!((back.secs, back.nsec), (FAT_EPOCH + 3, 250_000_000));
        // Out of range clamps.
        assert_eq!(
            from_dos(to_dos(Time::new(0, 0), 0).0, 0, 0, 0).secs,
            FAT_EPOCH
        );
        // An impossible date reads as 1980-01-01.
        assert_eq!(from_dos(0, 0, 0, 0).secs, FAT_EPOCH);
        // Time zone offset.
        let (d, tm, c) = to_dos(Time::new(1_793_534_400, 0), 3600);
        assert_eq!(from_dos(d, tm, c, 3600).secs, 1_793_534_400);
    }
}
