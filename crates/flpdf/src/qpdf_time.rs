//! qpdf correspondence: `QUtil::QPDFTime`, `get_current_qpdf_time`, and `qpdf_time_to_pdf_time`.
//! Source details: `include/qpdf/QUtil.hh:227-261`, `libqpdf/QUtil.cc:868-934`.

#![allow(unsafe_code)]

use std::sync::OnceLock;

/// qpdf's portable representation of a local wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QpdfTime {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    /// Minutes before UTC, matching qpdf's `tz_delta` convention.
    tz_delta: i32,
}

impl QpdfTime {
    const fn new(
        year: i32,
        month: i32,
        day: i32,
        hour: i32,
        minute: i32,
        second: i32,
        tz_delta: i32,
    ) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            tz_delta,
        }
    }
}

/// Format a qpdf time using qpdf's PDF-date sign convention.
fn format_qpdf_time(qtm: QpdfTime) -> Vec<u8> {
    let mut result = String::with_capacity(32);
    result.push_str("D:");
    use std::fmt::Write;
    write!(
        result,
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        qtm.year, qtm.month, qtm.day, qtm.hour, qtm.minute, qtm.second
    )
    .expect("writing to a String cannot fail");

    if qtm.tz_delta == 0 {
        result.push('Z');
    } else {
        let (sign, absolute_delta) = if qtm.tz_delta < 0 {
            ('+', i64::from(qtm.tz_delta).abs())
        } else {
            ('-', i64::from(qtm.tz_delta))
        };
        write!(
            result,
            "{sign}{:02}'{:02}'",
            absolute_delta / 60,
            absolute_delta % 60
        )
        .expect("writing to a String cannot fail");
    }

    result.into_bytes()
}

/// Return qpdf's process-stable default attachment date.
pub(crate) fn default_pdf_date() -> &'static [u8] {
    static DEFAULT_DATE: OnceLock<Vec<u8>> = OnceLock::new();
    DEFAULT_DATE
        .get_or_init(|| format_qpdf_time(current_qpdf_time()))
        .as_slice()
}

#[cfg(unix)]
unsafe extern "C" {
    fn tzset();
}

#[cfg(unix)]
fn current_qpdf_time() -> QpdfTime {
    let now = {
        // SAFETY: `time` accepts a null output pointer when only the return
        // value is requested, as in qpdf's `time(nullptr)` call.
        unsafe { libc::time(std::ptr::null_mut()) }
    };
    {
        // SAFETY: `tzset` refreshes the C library's process timezone state;
        // it takes no arguments and has no Rust-visible aliasing contract.
        unsafe { tzset() };
    }

    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let local_ptr = {
        // SAFETY: `local` points to writable storage for one `libc::tm`, and
        // `now` is a valid value returned by `time`.
        unsafe { libc::localtime_r(&now, local.as_mut_ptr()) }
    };
    assert!(!local_ptr.is_null(), "localtime_r failed");
    let local = {
        // SAFETY: a non-null `localtime_r` result means the supplied storage
        // was initialized with the converted calendar value.
        unsafe { local.assume_init() }
    };

    QpdfTime::new(
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
        // qpdf stores minutes before UTC, while `tm_gmtoff` is seconds after
        // UTC (`libqpdf/QUtil.cc:892-894`).
        -(local.tm_gmtoff / 60) as i32,
    )
}

#[cfg(windows)]
fn current_qpdf_time() -> QpdfTime {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

    let mut local = SYSTEMTIME::default();
    let mut timezone = TIME_ZONE_INFORMATION::default();
    // SAFETY: both Windows APIs receive pointers to initialized writable
    // structures of the exact types declared by the Windows SDK. qpdf uses
    // the same APIs and takes `TIME_ZONE_INFORMATION::Bias` directly.
    unsafe {
        GetLocalTime(&mut local);
        let _ = GetTimeZoneInformation(&mut timezone);
    }

    QpdfTime::new(
        i32::from(local.wYear),
        i32::from(local.wMonth),
        i32::from(local.wDay),
        i32::from(local.wHour),
        i32::from(local.wMinute),
        i32::from(local.wSecond),
        timezone.Bias,
    )
}

#[cfg(not(any(unix, windows)))]
fn current_qpdf_time() -> QpdfTime {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (now / 86_400) as i64;
    let seconds_today = now % 86_400;
    let (year, month, day) = civil_from_days(days);
    QpdfTime::new(
        i32::from(year),
        i32::from(month),
        i32::from(day),
        (seconds_today / 3_600) as i32,
        ((seconds_today / 60) % 60) as i32,
        (seconds_today % 60) as i32,
        0,
    )
}

#[cfg(not(any(unix, windows)))]
fn civil_from_days(days_since_epoch: i64) -> (u16, u8, u8) {
    // Howard Hinnant's proleptic Gregorian conversion, with the Unix epoch
    // at 1970-01-01. This is only a fallback for targets without qpdf's
    // Unix or Windows local-time APIs.
    let shifted = days_since_epoch + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year as u16, month as u8, day as u8)
}

#[cfg(test)]
mod tests {
    use super::{default_pdf_date, format_qpdf_time, QpdfTime};

    #[test]
    fn formats_zero_offset_as_utc() {
        assert_eq!(
            format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, 0)),
            b"D:20260820224733Z"
        );
    }

    #[test]
    fn formats_negative_qpdf_offset_as_positive_pdf_offset() {
        assert_eq!(
            format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, -540)),
            b"D:20260820224733+09'00'"
        );
    }

    #[test]
    fn formats_positive_qpdf_offset_as_negative_pdf_offset() {
        assert_eq!(
            format_qpdf_time(QpdfTime::new(2026, 8, 20, 22, 47, 33, 60)),
            b"D:20260820224733-01'00'"
        );
    }

    #[test]
    fn caches_the_default_date() {
        assert_eq!(default_pdf_date(), default_pdf_date());
        assert!(default_pdf_date().starts_with(b"D:"));
    }
}
