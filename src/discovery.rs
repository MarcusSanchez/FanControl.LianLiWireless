//! The receiver's answer to a discovery request.
//!
//! The reply starts with a four-byte header and continues with one 42-byte
//! record per device the receiver can hear, in the order the receiver
//! lists them. The dongle itself appears as a master record; everything
//! else is a device record with its fans' speeds and duties.

use std::fmt;

/// Bytes before the first record.
pub const HEADER_LEN: usize = 4;

/// Bytes in one record.
pub const RECORD_LEN: usize = 42;

/// Fans a record can describe.
pub const FANS_PER_GROUP: usize = 4;

const SEND_RF: u8 = 0x10;
const MARKER: u8 = 0x1C;
const MASTER: u8 = 0xFF;

/// Why a reply could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Fewer than [`HEADER_LEN`] bytes.
    Short,
    /// The command byte is not a discovery reply.
    WrongCommand(u8),
    /// The reply ends before the records its header promises.
    Truncated {
        /// Bytes the header implies.
        expected: usize,
        /// Bytes received.
        actual: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => write!(f, "discovery reply shorter than its header"),
            Self::WrongCommand(command) => {
                write!(f, "discovery reply has command byte 0x{command:02x}")
            }
            Self::Truncated { expected, actual } => {
                write!(f, "discovery reply truncated: {actual} of {expected} bytes")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The dongle's own entry in a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Master {
    /// Address of the dongle.
    pub mac: [u8; 6],
    /// Channel it reports.
    pub channel: u8,
}

/// One group of fans, or another device bound to a dongle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    /// Address of the group's receiver.
    pub mac: [u8; 6],
    /// Address of the dongle the group is bound to.
    pub master_mac: [u8; 6],
    /// Channel the group is on.
    pub channel: u8,
    /// Receiver type, named in every radio frame sent to the group.
    pub receiver: u8,
    /// Kind of device: 0 for fans, other values for lighting and coolers.
    pub device_type: u8,
    /// Fans attached, at most [`FANS_PER_GROUP`].
    pub fan_count: u8,
    /// Whether the fans chain from the right, which reverses their slots.
    pub right_attach: bool,
    /// Lighting effect the receiver is running.
    pub effect: [u8; 4],
    /// Fan model byte for each slot.
    pub fan_types: [u8; 4],
    /// Speed of each fan in revolutions per minute.
    pub rpm: [u16; FANS_PER_GROUP],
    /// Duty the receiver is applying to each fan, 0 to 255.
    pub duty: [u8; FANS_PER_GROUP],
    /// Sequence number of the last command the receiver took.
    pub sequence: u8,
    /// Whether a PWM line is connected to the receiver.
    pub pwm_line: bool,
    /// Whether the receiver mirrors the motherboard's lighting.
    pub light_sync: bool,
}

/// A parsed reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Devices the receiver says it can hear, including masters.
    pub reported: u8,
    /// Dongles seen, in reply order.
    pub masters: Vec<Master>,
    /// Devices seen, in reply order, malformed records left out.
    pub devices: Vec<Device>,
    /// Records left out for a bad marker or a zero address.
    pub skipped: u8,
}

/// Reads a reply to a discovery request that asked for `pages` pages.
///
/// The header's device count is held to what `pages` pages can carry.
/// Records that fail their marker or carry a zero address are skipped
/// without disturbing the order of the rest.
pub fn parse_reply(reply: &[u8], pages: u8) -> Result<Reply, Error> {
    if reply.len() < HEADER_LEN {
        return Err(Error::Short);
    }
    if reply[0] != SEND_RF {
        return Err(Error::WrongCommand(reply[0]));
    }
    let reported = reply[1];
    let capacity = usize::from(pages.max(1)) * crate::frame::RECORDS_PER_PAGE;
    let records = usize::from(reported).min(capacity);
    let expected = HEADER_LEN + records * RECORD_LEN;
    if reply.len() < expected {
        return Err(Error::Truncated {
            expected,
            actual: reply.len(),
        });
    }
    let mut masters = Vec::new();
    let mut devices = Vec::new();
    let mut skipped = 0;
    for record in reply[HEADER_LEN..expected].as_chunks::<RECORD_LEN>().0 {
        if let Some(master) = parse_master(record) {
            masters.push(master);
        } else if let Some(device) = parse_device(record) {
            devices.push(device);
        } else {
            skipped += 1;
        }
    }
    Ok(Reply {
        reported,
        masters,
        devices,
        skipped,
    })
}

/// Reads a record as a dongle's own entry.
pub fn parse_master(record: &[u8]) -> Option<Master> {
    if record.len() < RECORD_LEN || record[41] != MARKER || record[18] != MASTER {
        return None;
    }
    let mac = mac_at(record, 0)?;
    Some(Master {
        mac,
        channel: record[12],
    })
}

/// Reads a record as a bound device.
pub fn parse_device(record: &[u8]) -> Option<Device> {
    if record.len() < RECORD_LEN || record[41] != MARKER || record[18] == MASTER {
        return None;
    }
    let mac = mac_at(record, 0)?;
    let mut master_mac = [0; 6];
    master_mac.copy_from_slice(&record[6..12]);
    let raw_count = record[19];
    let (fan_count, right_attach) = if raw_count >= 10 {
        ((raw_count - 10).min(FANS_PER_GROUP as u8), true)
    } else {
        (raw_count.min(FANS_PER_GROUP as u8), false)
    };
    let mut effect = [0; 4];
    effect.copy_from_slice(&record[20..24]);
    let mut fan_types = [0; 4];
    fan_types.copy_from_slice(&record[24..28]);
    let mut rpm = [0; FANS_PER_GROUP];
    for (slot, value) in rpm.iter_mut().enumerate() {
        let at = 28 + slot * 2;
        *value = u16::from_be_bytes([record[at] & 0x0F, record[at + 1]]);
    }
    let mut duty = [0; FANS_PER_GROUP];
    duty.copy_from_slice(&record[36..40]);
    Some(Device {
        mac,
        master_mac,
        channel: record[12],
        receiver: record[13],
        device_type: record[18],
        fan_count,
        right_attach,
        effect,
        fan_types,
        rpm,
        duty,
        sequence: record[40],
        pwm_line: record[28] & 0x20 != 0,
        light_sync: record[28] & 0x40 != 0,
    })
}

fn mac_at(record: &[u8], at: usize) -> Option<[u8; 6]> {
    let mut mac = [0; 6];
    mac.copy_from_slice(&record[at..at + 6]);
    (mac != [0; 6]).then_some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DONGLE: [u8; 6] = [0xbf, 0x3d, 0xca, 0xe5, 0x66, 0xe4];
    const GROUP_A: [u8; 6] = [0x7c, 0x9c, 0x06, 0xf5, 0x17, 0xe1];
    const GROUP_B: [u8; 6] = [0x99, 0xdb, 0xc8, 0xe5, 0x66, 0xe1];
    const STRIMER: [u8; 6] = [0x4d, 0xf1, 0xd7, 0xe5, 0x66, 0xe1];

    fn record(mac: [u8; 6], device_type: u8, fan_count: u8) -> [u8; RECORD_LEN] {
        let mut r = [0; RECORD_LEN];
        r[0..6].copy_from_slice(&mac);
        r[6..12].copy_from_slice(&DONGLE);
        r[12] = 8;
        r[13] = 2;
        r[18] = device_type;
        r[19] = fan_count;
        r[41] = MARKER;
        r
    }

    fn master_record() -> [u8; RECORD_LEN] {
        record(DONGLE, MASTER, 0)
    }

    fn group_a() -> [u8; RECORD_LEN] {
        let mut r = record(GROUP_A, 0, 13);
        r[13] = 2;
        r[20..24].copy_from_slice(&[1, 2, 3, 4]);
        r[24..28].copy_from_slice(&[43, 43, 43, 0]);
        r[28..36].copy_from_slice(&[0x66, 0x72, 0x06, 0x80, 0x07, 0x1c, 0x00, 0x00]);
        r[36..40].copy_from_slice(&[201, 201, 201, 0]);
        r[40] = 17;
        r
    }

    fn group_b() -> [u8; RECORD_LEN] {
        let mut r = record(GROUP_B, 0, 2);
        r[13] = 6;
        r[24..28].copy_from_slice(&[45, 45, 0, 0]);
        r[28..36].copy_from_slice(&[0x06, 0x9a, 0x06, 0xa4, 0, 0, 0, 0]);
        r[36..40].copy_from_slice(&[206, 206, 0, 0]);
        r[40] = 3;
        r
    }

    fn reply(records: &[[u8; RECORD_LEN]]) -> Vec<u8> {
        let mut reply = vec![SEND_RF, records.len() as u8, 0, 0];
        for r in records {
            reply.extend_from_slice(r);
        }
        reply
    }

    #[test]
    fn reply_separates_masters_from_devices_in_order() {
        let parsed = parse_reply(
            &reply(&[master_record(), group_a(), record(STRIMER, 4, 0), group_b()]),
            1,
        )
        .unwrap();
        assert_eq!(parsed.reported, 4);
        assert_eq!(
            parsed.masters,
            vec![Master {
                mac: DONGLE,
                channel: 8
            }]
        );
        let macs: Vec<[u8; 6]> = parsed.devices.iter().map(|d| d.mac).collect();
        assert_eq!(macs, vec![GROUP_A, STRIMER, GROUP_B]);
    }

    #[test]
    fn device_fields_are_read_from_their_offsets() {
        let d = parse_device(&group_a()).unwrap();
        assert_eq!(d.mac, GROUP_A);
        assert_eq!(d.master_mac, DONGLE);
        assert_eq!(d.channel, 8);
        assert_eq!(d.receiver, 2);
        assert_eq!(d.device_type, 0);
        assert_eq!(d.effect, [1, 2, 3, 4]);
        assert_eq!(d.fan_types, [43, 43, 43, 0]);
        assert_eq!(d.duty, [201, 201, 201, 0]);
        assert_eq!(d.sequence, 17);
    }

    #[test]
    fn rpm_masks_the_high_byte_and_reads_the_flags() {
        let d = parse_device(&group_a()).unwrap();
        assert_eq!(d.rpm, [0x672, 0x680, 0x71c, 0]);
        assert!(d.pwm_line);
        assert!(d.light_sync);
        let d = parse_device(&group_b()).unwrap();
        assert_eq!(d.rpm, [0x69a, 0x6a4, 0, 0]);
        assert!(!d.pwm_line);
        assert!(!d.light_sync);
    }

    #[test]
    fn fan_count_over_ten_means_right_attach() {
        let d = parse_device(&group_a()).unwrap();
        assert_eq!(d.fan_count, 3);
        assert!(d.right_attach);
        let d = parse_device(&group_b()).unwrap();
        assert_eq!(d.fan_count, 2);
        assert!(!d.right_attach);
    }

    #[test]
    fn fan_count_is_held_to_four() {
        assert_eq!(parse_device(&record(GROUP_A, 0, 7)).unwrap().fan_count, 4);
        let d = parse_device(&record(GROUP_A, 0, 19)).unwrap();
        assert_eq!(d.fan_count, 4);
        assert!(d.right_attach);
    }

    #[test]
    fn lighting_devices_are_devices_too() {
        let d = parse_device(&record(STRIMER, 4, 0)).unwrap();
        assert_eq!(d.device_type, 4);
        assert_eq!(d.fan_count, 0);
    }

    #[test]
    fn master_record_is_not_a_device() {
        assert!(parse_device(&master_record()).is_none());
        assert!(parse_master(&group_a()).is_none());
    }

    #[test]
    fn bad_marker_and_zero_address_are_skipped() {
        let mut bad_marker = group_a();
        bad_marker[41] = 0;
        let mut zero_mac = group_b();
        zero_mac[0..6].fill(0);
        let mut zero_master = master_record();
        zero_master[0..6].fill(0);
        assert!(parse_device(&bad_marker).is_none());
        assert!(parse_device(&zero_mac).is_none());
        assert!(parse_master(&zero_master).is_none());
        let parsed =
            parse_reply(&reply(&[bad_marker, zero_master, group_b(), zero_mac]), 1).unwrap();
        assert_eq!(parsed.reported, 4);
        assert!(parsed.masters.is_empty());
        assert_eq!(parsed.devices.len(), 1);
        assert_eq!(parsed.devices[0].mac, GROUP_B);
        assert_eq!(parsed.skipped, 3);
        assert_eq!(parse_reply(&reply(&[group_a()]), 1).unwrap().skipped, 0);
    }

    #[test]
    fn short_record_is_rejected() {
        assert!(parse_device(&group_a()[..41]).is_none());
        assert!(parse_master(&master_record()[..41]).is_none());
    }

    #[test]
    fn reply_header_is_checked() {
        assert_eq!(parse_reply(&[0x10, 0, 0], 1), Err(Error::Short));
        assert_eq!(
            parse_reply(&[0x11, 0, 0, 0], 1),
            Err(Error::WrongCommand(0x11))
        );
        let empty = parse_reply(&[0x10, 0, 0, 0], 1).unwrap();
        assert_eq!(empty.reported, 0);
        assert!(empty.devices.is_empty());
    }

    #[test]
    fn reply_truncated_before_its_records_is_rejected() {
        let full = reply(&[group_a(), group_b()]);
        assert_eq!(
            parse_reply(&full[..full.len() - 1], 1),
            Err(Error::Truncated {
                expected: 88,
                actual: 87
            })
        );
        assert!(parse_reply(&full, 1).is_ok());
    }

    #[test]
    fn reply_count_is_held_to_the_pages_asked_for() {
        let mut short = reply(&[group_a()]);
        short[1] = 200;
        assert_eq!(
            parse_reply(&short, 1),
            Err(Error::Truncated {
                expected: 424,
                actual: 46
            })
        );
        let mut ten = vec![SEND_RF, 200, 0, 0];
        for _ in 0..10 {
            ten.extend_from_slice(&group_b());
        }
        let parsed = parse_reply(&ten, 1).unwrap();
        assert_eq!(parsed.reported, 200);
        assert_eq!(parsed.devices.len(), 10);
        assert_eq!(parse_reply(&ten, 0).unwrap().devices.len(), 10);
    }

    #[test]
    fn extra_bytes_after_the_records_are_ignored() {
        let mut padded = reply(&[group_a()]);
        padded.extend_from_slice(&[0xAA; 100]);
        assert_eq!(parse_reply(&padded, 1).unwrap().devices.len(), 1);
    }
}
