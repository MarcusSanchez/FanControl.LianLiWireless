//! The command that sets a group's fan duties.
//!
//! A duty is 0 to 255. Each group has a lowest duty its fans will run at,
//! set by the fan model; anything above zero and below it is raised to it.
//! Groups that chain from the right take their slots in reverse.

use crate::discovery::{Device, FANS_PER_GROUP};
use crate::frame::{RfPayload, RF_PAYLOAD_LEN};

/// Highest duty a fan takes.
pub const MAX_DUTY: u8 = 255;

/// Difference between a sent duty and a reported one that still counts as
/// applied, above [`EXACT_BELOW`].
pub const TOLERANCE: u8 = 5;

/// Duties at or below this must be reported back exactly.
pub const EXACT_BELOW: u8 = 10;

const SELECT: u8 = 0x12;
const PWM: u8 = 0x10;

/// The lowest non-zero duty this device's fans will run at.
pub fn min_duty(device: &Device) -> u8 {
    (u16::from(min_percent(device)) * u16::from(MAX_DUTY) / 100) as u8
}

/// The fan model byte, for devices whose kind is not fixed by their
/// device type.
fn model(device: &Device) -> Option<u8> {
    match device.device_type {
        1..=11 | 65 | 66 | 88 => None,
        _ => device.fan_types.iter().copied().find(|b| *b != 0),
    }
}

fn min_percent(device: &Device) -> u8 {
    match device.device_type {
        1..=9 | 65 | 88 => 0,
        10 | 11 | 66 => 10,
        _ => match model(device) {
            Some(20..=26) | Some(59..=62) => 14,
            Some(28..=31) | Some(36..=39) | Some(51..=58) => 11,
            Some(63) => 8,
            _ => 10,
        },
    }
}

fn is_cooler(device: &Device) -> bool {
    matches!(device.device_type, 10 | 11)
}

fn filters_duty(device: &Device) -> bool {
    matches!(model(device), Some(40..=42) | Some(126) | Some(127))
}

/// Turns wanted duties into the ones sent to a device.
///
/// Slots past the device's fan count go to zero, except a cooler's pump in
/// the last slot. Non-zero duties below the device's minimum are raised to
/// it. CL-series fans skip the duties 153 to 155. Right-attached chains
/// take their slots in reverse.
pub fn prepare(device: &Device, wanted: [u8; FANS_PER_GROUP]) -> [u8; FANS_PER_GROUP] {
    let floor = min_duty(device);
    let mut duty = wanted;
    for (slot, value) in duty.iter_mut().enumerate() {
        let pump = slot == FANS_PER_GROUP - 1 && is_cooler(device);
        if slot >= usize::from(device.fan_count) && !pump {
            *value = 0;
            continue;
        }
        if *value > 0 && *value < floor {
            *value = floor;
        }
        if filters_duty(device) {
            *value = match *value {
                153 | 154 => 152,
                155 => 156,
                other => other,
            };
        }
    }
    if device.right_attach {
        let fans = usize::from(device.fan_count).min(FANS_PER_GROUP);
        if fans > 1 {
            duty[..fans].reverse();
        }
    }
    duty
}

/// The radio payload that applies `duty` to `device`.
///
/// `duty` is taken as returned by [`prepare`]. `channel` is the dongle's
/// channel and `slot` the device's position from [`slot`].
pub fn payload(
    device: &Device,
    master_mac: &[u8; 6],
    channel: u8,
    slot: u8,
    duty: [u8; FANS_PER_GROUP],
) -> RfPayload {
    let mut data = [0; RF_PAYLOAD_LEN];
    data[0] = SELECT;
    data[1] = PWM;
    data[2..8].copy_from_slice(&device.mac);
    data[8..14].copy_from_slice(master_mac);
    data[14] = device.receiver;
    data[15] = channel;
    data[16] = slot;
    data[17..21].copy_from_slice(&duty);
    data
}

/// The 1-based position of `mac` among the devices bound to `master_mac`,
/// counted in the order `devices` are listed. A device not in the list is
/// given position 1.
pub fn slot(devices: &[Device], master_mac: &[u8; 6], mac: &[u8; 6]) -> u8 {
    devices
        .iter()
        .filter(|d| d.master_mac == *master_mac)
        .position(|d| d.mac == *mac)
        .map(|index| (index + 1) as u8)
        .unwrap_or(1)
}

/// Whether the duties a device reports match the ones sent to it.
pub fn acknowledged(reported: &[u8; FANS_PER_GROUP], sent: &[u8; FANS_PER_GROUP]) -> bool {
    reported.iter().zip(sent).all(|(r, s)| {
        if *s <= EXACT_BELOW {
            r == s
        } else {
            r.abs_diff(*s) <= TOLERANCE
        }
    })
}

/// Whether the duties a device reports differ enough from the wanted ones
/// to send them again.
pub fn differs(reported: &[u8; FANS_PER_GROUP], wanted: &[u8; FANS_PER_GROUP]) -> bool {
    !acknowledged(reported, wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 6] = [9; 6];

    fn device(mac: u8, device_type: u8, model: u8, fans: u8, right_attach: bool) -> Device {
        Device {
            mac: [mac; 6],
            master_mac: MASTER,
            channel: 8,
            receiver: 2,
            device_type,
            fan_count: fans,
            right_attach,
            effect: [0; 4],
            fan_types: [model, model, model, 0],
            rpm: [0; 4],
            duty: [0; 4],
            sequence: 0,
            pwm_line: false,
            light_sync: false,
        }
    }

    #[test]
    fn minimum_duty_follows_the_fan_model() {
        assert_eq!(min_duty(&device(1, 0, 20, 3, false)), 35);
        assert_eq!(min_duty(&device(1, 0, 43, 3, false)), 25);
        assert_eq!(min_duty(&device(1, 0, 36, 3, false)), 28);
        assert_eq!(min_duty(&device(1, 0, 63, 3, false)), 20);
        assert_eq!(min_duty(&device(1, 0, 0, 3, false)), 25);
        assert_eq!(min_duty(&device(1, 4, 0, 0, false)), 0);
        assert_eq!(min_duty(&device(1, 10, 0, 2, false)), 25);
    }

    #[test]
    fn prepare_zeroes_unused_slots_and_raises_to_the_floor() {
        let d = device(1, 0, 20, 3, false);
        assert_eq!(prepare(&d, [0, 1, 255, 255]), [0, 35, 255, 0]);
        let d = device(1, 0, 43, 3, false);
        assert_eq!(prepare(&d, [1, 24, 25, 26]), [25, 25, 25, 0]);
        assert_eq!(prepare(&d, [26, 200, 0, 0]), [26, 200, 0, 0]);
    }

    #[test]
    fn prepare_filters_cl_series_duties() {
        let d = device(1, 0, 40, 3, false);
        assert_eq!(prepare(&d, [153, 154, 155, 255]), [152, 152, 156, 0]);
        let d = device(1, 0, 43, 3, false);
        assert_eq!(prepare(&d, [153, 154, 155, 0]), [153, 154, 155, 0]);
        let d = device(1, 12, 126, 3, false);
        assert_eq!(prepare(&d, [153, 154, 155, 0]), [152, 152, 156, 0]);
        let d = device(1, 10, 40, 3, false);
        assert_eq!(prepare(&d, [153, 154, 155, 255]), [153, 154, 155, 255]);
    }

    #[test]
    fn prepare_reverses_right_attached_chains() {
        let d = device(1, 0, 36, 3, true);
        assert_eq!(prepare(&d, [100, 150, 200, 255]), [200, 150, 100, 0]);
        let d = device(1, 0, 36, 2, true);
        assert_eq!(prepare(&d, [100, 150, 200, 255]), [150, 100, 0, 0]);
        let d = device(1, 0, 36, 1, true);
        assert_eq!(prepare(&d, [100, 150, 200, 255]), [100, 0, 0, 0]);
    }

    #[test]
    fn prepare_keeps_a_coolers_pump_slot() {
        let d = device(1, 10, 0, 2, false);
        assert_eq!(prepare(&d, [255; 4]), [255, 255, 0, 255]);
    }

    #[test]
    fn payload_has_the_command_layout() {
        let d = device(1, 0, 43, 3, false);
        let p = payload(&d, &MASTER, 8, 3, [6, 6, 6, 6]);
        assert_eq!(
            &p[..21],
            &[0x12, 0x10, 1, 1, 1, 1, 1, 1, 9, 9, 9, 9, 9, 9, 2, 8, 3, 6, 6, 6, 6]
        );
        assert_eq!(p.len(), 240);
        assert!(p[21..].iter().all(|b| *b == 0));
    }

    #[test]
    fn slot_counts_bound_devices_in_list_order() {
        let mut foreign = device(3, 0, 43, 3, false);
        foreign.master_mac = [7; 6];
        let devices = [
            device(1, 0, 43, 3, false),
            foreign,
            device(4, 4, 0, 0, false),
            device(2, 0, 43, 2, false),
        ];
        assert_eq!(slot(&devices, &MASTER, &[1; 6]), 1);
        assert_eq!(slot(&devices, &MASTER, &[4; 6]), 2);
        assert_eq!(slot(&devices, &MASTER, &[2; 6]), 3);
        assert_eq!(slot(&devices, &MASTER, &[3; 6]), 1);
        assert_eq!(slot(&devices, &MASTER, &[5; 6]), 1);
    }

    #[test]
    fn acknowledgement_allows_a_small_difference_above_ten() {
        assert!(acknowledged(&[205, 196, 201, 0], &[201, 201, 201, 0]));
        assert!(!acknowledged(&[207, 201, 201, 0], &[201, 201, 201, 0]));
        assert!(acknowledged(&[10, 0, 0, 0], &[10, 0, 0, 0]));
        assert!(!acknowledged(&[11, 0, 0, 0], &[10, 0, 0, 0]));
        assert!(!acknowledged(&[0, 0, 0, 0], &[5, 0, 0, 0]));
        assert!(differs(&[0, 0, 0, 0], &[5, 0, 0, 0]));
        assert!(!differs(&[201; 4], &[201; 4]));
    }
}
