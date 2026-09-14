//! The broadcast the fans expect once a second.
//!
//! The firmware treats the master clock as proof that a host is present.
//! Without it the fans fall back to running on their own, with the
//! occasional burst of speed. The block it carries also feeds the readouts
//! on fans that have screens; fans without them ignore it.

use crate::frame::{RfPayload, RF_PAYLOAD_LEN};
use std::time::Duration;

/// How often the heartbeat must go out.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Bytes in the block a heartbeat carries.
pub const BLOCK_LEN: usize = 220;

const SELECT: u8 = 0x12;
const CLOCK_SYNC: u8 = 0x14;
const UNSET: u8 = 0x14;

/// Values shown on fans with screens. Anything left at zero shows as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Readings {
    /// CPU temperature in degrees Celsius.
    pub cpu_temp: u8,
    /// CPU load in percent.
    pub cpu_load: u8,
    /// GPU temperature in degrees Celsius.
    pub gpu_temp: u8,
    /// GPU load in percent.
    pub gpu_load: u8,
    /// CPU clock in megahertz.
    pub cpu_mhz: u16,
    /// GPU clock in megahertz.
    pub gpu_mhz: u16,
}

/// Local date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clock {
    /// Full year.
    pub year: u16,
    /// Month, 1 to 12.
    pub month: u8,
    /// Day of the month, 1 to 31.
    pub day: u8,
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 59.
    pub second: u8,
}

/// The block a heartbeat carries.
pub fn block(readings: &Readings, clock: &Clock) -> [u8; BLOCK_LEN] {
    let mut block = [0; BLOCK_LEN];
    block[0] = readings.cpu_temp;
    block[1] = readings.cpu_load;
    block[2] = readings.gpu_temp;
    block[3] = readings.gpu_load;
    block[4..6].copy_from_slice(&readings.cpu_mhz.to_be_bytes());
    block[6..8].copy_from_slice(&readings.gpu_mhz.to_be_bytes());
    block[32..34].copy_from_slice(&clock.year.to_be_bytes());
    block[34] = clock.month;
    block[35] = clock.day;
    block[36] = clock.hour;
    block[37] = clock.minute;
    block[38] = clock.second;
    block
}

/// The radio payload of one heartbeat, broadcast from `master_mac`.
///
/// The first heartbeat after connecting is sent with `first` set: the
/// receivers then take the leading part of the block as unset and read only
/// its tail. Every later heartbeat carries the whole block.
pub fn payload(master_mac: &[u8; 6], block: &[u8; BLOCK_LEN], first: bool) -> RfPayload {
    let mut data = [0; RF_PAYLOAD_LEN];
    data[0] = SELECT;
    data[1] = CLOCK_SYNC;
    data[8..14].copy_from_slice(master_mac);
    if first {
        data[14..64].fill(UNSET);
        data[64..234].copy_from_slice(&block[50..]);
    } else {
        data[14..234].copy_from_slice(block);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 6] = [0xbf, 0x3d, 0xca, 0xe5, 0x66, 0xe4];

    fn readings() -> Readings {
        Readings {
            cpu_temp: 61,
            cpu_load: 17,
            gpu_temp: 48,
            gpu_load: 99,
            cpu_mhz: 5225,
            gpu_mhz: 2820,
        }
    }

    fn clock() -> Clock {
        Clock {
            year: 2031,
            month: 12,
            day: 25,
            hour: 23,
            minute: 59,
            second: 58,
        }
    }

    fn numbered() -> [u8; BLOCK_LEN] {
        let mut block = [0; BLOCK_LEN];
        for (i, b) in block.iter_mut().enumerate() {
            *b = i as u8;
        }
        block
    }

    #[test]
    fn block_places_readings_and_clock() {
        let b = block(&readings(), &clock());
        assert_eq!(&b[..8], &[61, 17, 48, 99, 0x14, 0x69, 0x0b, 0x04]);
        assert!(b[8..32].iter().all(|x| *x == 0));
        assert_eq!(&b[32..39], &[0x07, 0xef, 12, 25, 23, 59, 58]);
        assert!(b[39..].iter().all(|x| *x == 0));
    }

    #[test]
    fn empty_readings_give_an_all_zero_block_but_the_clock() {
        let b = block(&Readings::default(), &clock());
        assert!(b[..32].iter().all(|x| *x == 0));
        assert_eq!(&b[32..39], &[0x07, 0xef, 12, 25, 23, 59, 58]);
        assert!(block(&Readings::default(), &Clock::default())
            .iter()
            .all(|x| *x == 0));
    }

    #[test]
    fn steady_payload_carries_the_whole_block() {
        let p = payload(&MASTER, &numbered(), false);
        assert_eq!(&p[..2], &[0x12, 0x14]);
        assert!(p[2..8].iter().all(|x| *x == 0));
        assert_eq!(&p[8..14], &MASTER);
        assert_eq!(&p[14..234], &numbered()[..]);
        assert!(p[234..].iter().all(|x| *x == 0));
        assert_eq!(p.len(), 240);
    }

    #[test]
    fn first_payload_marks_the_head_unset_and_keeps_the_tail() {
        let p = payload(&MASTER, &numbered(), true);
        assert_eq!(&p[..2], &[0x12, 0x14]);
        assert_eq!(&p[8..14], &MASTER);
        assert!(p[14..64].iter().all(|x| *x == 0x14));
        assert_eq!(&p[64..234], &numbered()[50..]);
        assert_eq!(p[64], 50);
        assert_eq!(p[233], 219);
        assert!(p[234..].iter().all(|x| *x == 0));
    }

    #[test]
    fn interval_is_one_second() {
        assert_eq!(INTERVAL, Duration::from_secs(1));
    }
}
