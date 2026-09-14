//! The local date and time, for the heartbeat.

use crate::heartbeat::Clock;
use crate::win::{GetLocalTime, SYSTEMTIME};

/// The current local date and time.
pub fn local() -> Clock {
    let mut time = SYSTEMTIME::default();
    unsafe {
        GetLocalTime(&mut time);
    }
    Clock {
        year: time.wYear,
        month: time.wMonth as u8,
        day: time.wDay as u8,
        hour: time.wHour as u8,
        minute: time.wMinute as u8,
        second: time.wSecond as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_is_a_real_date() {
        let now = local();
        assert!(now.year >= 2025, "{now:?}");
        assert!((1..=12).contains(&now.month), "{now:?}");
        assert!((1..=31).contains(&now.day), "{now:?}");
        assert!(now.hour <= 23, "{now:?}");
        assert!(now.minute <= 59, "{now:?}");
        assert!(now.second <= 59, "{now:?}");
    }
}
