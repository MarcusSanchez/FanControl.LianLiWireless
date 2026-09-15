//! What is known about the devices bound to the dongle, kept up to date
//! from discovery replies.
//!
//! Devices keep the order they were first listed in, and a device unseen
//! for long enough drops out of that order until it is heard again. That
//! order gives each bound device its slot in speed commands. It is the
//! order the Linux driver for this dongle keeps, not the receiver's
//! reply order: devices heard in the same poll join by ascending address,
//! later arrivals go after everything already known, and a device that
//! returns after going offline rejoins at the end. Whether the firmware
//! reads the slot at all is unknown; keeping the same order as a driver
//! that works in the field is the safe choice.

use crate::discovery::{Device, Reply, FANS_PER_GROUP};
use crate::speed;
use std::time::{Duration, Instant};

/// How long a device can go unheard before it counts as offline.
pub const OFFLINE_AFTER: Duration = Duration::from_secs(15);

/// How long a bound device can stay offline before it is forgotten.
pub const FORGET_AFTER: Duration = Duration::from_secs(10 * 60);

/// How recent a sighting must be for its reported duties to acknowledge a
/// command.
pub const ACK_WINDOW: Duration = Duration::from_secs(3);

/// How long an acknowledged duty is left alone before it is sent again.
pub const KEEPALIVE: Duration = Duration::from_secs(1);

const AGREE: u32 = 3;

/// One device and what has been asked of it.
#[derive(Debug, Clone)]
pub struct Group {
    /// The device as last reported, with its identity and shape confirmed.
    pub device: Device,
    /// When the device was last heard.
    pub last_seen: Instant,
    /// Duties wanted on it, as they will go on the wire.
    pub target: Option<[u8; FANS_PER_GROUP]>,
    /// When the target was last sent.
    pub last_sent: Option<Instant>,
    /// Sends of the target since it was last seen applied.
    pub unacknowledged: u32,
    identity: Option<(Identity, u32)>,
    shape: Option<(Shape, u32)>,
}

/// Who a device belongs to and how it is addressed.
type Identity = ([u8; 6], u8, u8);

/// What a device is: kind, fan count, attachment side, fan models. A
/// single garbled record must not change these, since they decide which
/// slots are driven and what the duty floor is.
type Shape = (u8, u8, bool, [u8; 4]);

fn identity(device: &Device) -> Identity {
    (device.master_mac, device.channel, device.receiver)
}

fn shape(device: &Device) -> Shape {
    (
        device.device_type,
        device.fan_count,
        device.right_attach,
        device.fan_types,
    )
}

impl Group {
    fn new(device: Device, now: Instant) -> Self {
        Self {
            device,
            last_seen: now,
            target: None,
            last_sent: None,
            unacknowledged: 0,
            identity: None,
            shape: None,
        }
    }

    /// Whether the device has been heard within [`OFFLINE_AFTER`].
    pub fn online(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_seen) <= OFFLINE_AFTER
    }

    /// Whether the device is bound to `master_mac`.
    pub fn bound_to(&self, master_mac: &[u8; 6]) -> bool {
        self.device.master_mac == *master_mac
    }

    /// Whether the device has fans to drive.
    pub fn has_fans(&self) -> bool {
        self.device.device_type == 0 && self.device.fan_count > 0
    }

    /// Whether the last sighting is fresh and reports the target applied.
    pub fn acknowledged(&self, now: Instant) -> bool {
        self.target.is_some_and(|target| {
            now.saturating_duration_since(self.last_seen) <= ACK_WINDOW
                && speed::acknowledged(&self.device.duty, &target)
        })
    }

    /// Whether the target should go out now: never sent, reported
    /// differently from what is wanted, or sent [`KEEPALIVE`] or more ago.
    pub fn due(&self, now: Instant) -> bool {
        let Some(target) = self.target else {
            return false;
        };
        match self.last_sent {
            None => true,
            Some(sent) => {
                speed::differs(&self.device.duty, &target)
                    || now.saturating_duration_since(sent) >= KEEPALIVE
            }
        }
    }

    fn take_sighting(&mut self, seen: &Device, now: Instant) {
        if !self.online(now) {
            self.device = *seen;
            self.identity = None;
            self.shape = None;
        } else {
            let kept = self.device;
            self.device = *seen;
            self.device.master_mac = kept.master_mac;
            self.device.channel = kept.channel;
            self.device.receiver = kept.receiver;
            self.device.device_type = kept.device_type;
            self.device.fan_count = kept.fan_count;
            self.device.right_attach = kept.right_attach;
            self.device.fan_types = kept.fan_types;
            if identity(seen) == identity(&kept) {
                self.identity = None;
            } else if let Some(agreed) = agree(&mut self.identity, identity(seen)) {
                (
                    self.device.master_mac,
                    self.device.channel,
                    self.device.receiver,
                ) = agreed;
            }
            if shape(seen) == shape(&kept) {
                self.shape = None;
            } else if let Some(agreed) = agree(&mut self.shape, shape(seen)) {
                (
                    self.device.device_type,
                    self.device.fan_count,
                    self.device.right_attach,
                    self.device.fan_types,
                ) = agreed;
            }
        }
        self.last_seen = now;
        if self.acknowledged(now) {
            self.unacknowledged = 0;
        }
    }
}

fn agree<T: Copy + Eq>(candidate: &mut Option<(T, u32)>, seen: T) -> Option<T> {
    match candidate {
        Some((value, count)) if *value == seen => {
            *count += 1;
            if *count >= AGREE {
                *candidate = None;
                Some(seen)
            } else {
                None
            }
        }
        _ => {
            *candidate = Some((seen, 1));
            None
        }
    }
}

/// Every device heard since start, in slot order.
#[derive(Debug, Clone)]
pub struct Tracker {
    master_mac: [u8; 6],
    groups: Vec<Group>,
    order: Vec<[u8; 6]>,
}

impl Tracker {
    /// A tracker for devices bound to `master_mac`.
    pub fn new(master_mac: [u8; 6]) -> Self {
        Self {
            master_mac,
            groups: Vec::new(),
            order: Vec::new(),
        }
    }

    /// The dongle's address.
    pub fn master_mac(&self) -> [u8; 6] {
        self.master_mac
    }

    /// Takes in one discovery reply.
    ///
    /// Devices heard for the first time join the order after those already
    /// in it, lowest address first. Devices that have gone offline leave
    /// the order and rejoin at the end when heard again.
    pub fn observe(&mut self, reply: &Reply, now: Instant) {
        for seen in &reply.devices {
            match self.groups.iter_mut().find(|g| g.device.mac == seen.mac) {
                Some(group) => group.take_sighting(seen, now),
                None => self.groups.push(Group::new(*seen, now)),
            }
        }
        self.rebuild(now);
    }

    /// Drops offline devices from the order. Forgets offline devices that
    /// were never bound to the dongle at once, and bound ones after
    /// [`FORGET_AFTER`]; returns the addresses forgotten.
    pub fn sweep(&mut self, now: Instant) -> Vec<[u8; 6]> {
        let master = self.master_mac;
        let mut forgotten = Vec::new();
        self.groups.retain(|g| {
            let keep = g.online(now)
                || (g.bound_to(&master)
                    && now.saturating_duration_since(g.last_seen) < FORGET_AFTER);
            if !keep {
                forgotten.push(g.device.mac);
            }
            keep
        });
        self.rebuild(now);
        forgotten
    }

    fn rebuild(&mut self, now: Instant) {
        let mut order: Vec<[u8; 6]> = self
            .order
            .iter()
            .copied()
            .filter(|mac| self.group(mac).is_some_and(|g| g.online(now)))
            .collect();
        let mut joining: Vec<[u8; 6]> = self
            .groups
            .iter()
            .filter(|g| g.online(now) && !order.contains(&g.device.mac))
            .map(|g| g.device.mac)
            .collect();
        joining.sort_unstable();
        order.extend(joining);
        self.order = order;
    }

    /// The device with this address, whether online or not.
    pub fn group(&self, mac: &[u8; 6]) -> Option<&Group> {
        self.groups.iter().find(|g| g.device.mac == *mac)
    }

    /// Every device known, in the order first heard.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Online devices in slot order.
    pub fn online(&self) -> impl Iterator<Item = &Group> {
        self.order.iter().filter_map(|mac| self.group(mac))
    }

    /// Online fan groups bound to the dongle, in slot order.
    pub fn fans(&self) -> impl Iterator<Item = &Group> {
        self.online()
            .filter(|g| g.bound_to(&self.master_mac) && g.has_fans())
    }

    /// The slot a speed command names for this device.
    pub fn slot(&self, mac: &[u8; 6]) -> u8 {
        let devices: Vec<Device> = self.online().map(|g| g.device).collect();
        speed::slot(&devices, &self.master_mac, mac)
    }

    /// Sets the duties wanted on a device and returns them as they will be
    /// sent, or `None` for a device never heard.
    pub fn set_target(
        &mut self,
        mac: &[u8; 6],
        wanted: [u8; FANS_PER_GROUP],
    ) -> Option<[u8; FANS_PER_GROUP]> {
        let group = self.groups.iter_mut().find(|g| g.device.mac == *mac)?;
        let prepared = speed::prepare(&group.device, wanted);
        if group.target != Some(prepared) {
            group.target = Some(prepared);
            group.last_sent = None;
            group.unacknowledged = 0;
        }
        Some(prepared)
    }

    /// Records that the device's target went out.
    pub fn sent(&mut self, mac: &[u8; 6], now: Instant) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.device.mac == *mac) {
            group.last_sent = Some(now);
            group.unacknowledged += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 6] = [9; 6];
    const OTHER: [u8; 6] = [7; 6];

    fn device(mac: [u8; 6], master: [u8; 6], device_type: u8, fans: u8) -> Device {
        Device {
            mac,
            master_mac: master,
            channel: 8,
            receiver: 2,
            device_type,
            fan_count: fans,
            right_attach: false,
            effect: [0; 4],
            fan_types: [43, 43, 43, 0],
            rpm: [1000; 4],
            duty: [100, 100, 100, 0],
            sequence: 1,
            pwm_line: false,
            light_sync: false,
        }
    }

    fn reply(devices: &[Device]) -> Reply {
        Reply {
            reported: devices.len() as u8,
            masters: Vec::new(),
            devices: devices.to_vec(),
            skipped: 0,
        }
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn first_sightings_join_in_address_order() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        let c = device([3; 6], MASTER, 0, 3);
        let a = device([1; 6], MASTER, 0, 3);
        let b = device([2; 6], MASTER, 0, 2);
        t.observe(&reply(&[c, a, b]), base);
        let order: Vec<[u8; 6]> = t.online().map(|g| g.device.mac).collect();
        assert_eq!(order, vec![[1; 6], [2; 6], [3; 6]]);
        assert_eq!(t.slot(&[1; 6]), 1);
        assert_eq!(t.slot(&[2; 6]), 2);
        assert_eq!(t.slot(&[3; 6]), 3);
    }

    #[test]
    fn later_arrivals_go_after_the_existing_order() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([5; 6], MASTER, 0, 3)]), base);
        t.observe(
            &reply(&[
                device([5; 6], MASTER, 0, 3),
                device([4; 6], MASTER, 0, 3),
                device([1; 6], MASTER, 0, 3),
            ]),
            at(base, 1),
        );
        let order: Vec<[u8; 6]> = t.online().map(|g| g.device.mac).collect();
        assert_eq!(order, vec![[5; 6], [1; 6], [4; 6]]);
    }

    #[test]
    fn live_fields_update_on_every_sighting() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        let mut seen = device([1; 6], MASTER, 0, 3);
        seen.rpm = [1500; 4];
        seen.duty = [200, 200, 200, 0];
        seen.sequence = 9;
        t.observe(&reply(&[seen]), at(base, 1));
        let g = t.group(&[1; 6]).unwrap();
        assert_eq!(g.device.rpm, [1500; 4]);
        assert_eq!(g.device.duty, [200, 200, 200, 0]);
        assert_eq!(g.device.sequence, 9);
        assert_eq!(g.last_seen, at(base, 1));
    }

    #[test]
    fn identity_changes_need_three_agreeing_sightings() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        let mut moved = device([1; 6], MASTER, 0, 3);
        moved.channel = 12;
        t.observe(&reply(&[moved]), at(base, 1));
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 8);
        t.observe(&reply(&[moved]), at(base, 2));
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 8);
        t.observe(&reply(&[moved]), at(base, 3));
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 12);
    }

    #[test]
    fn a_disagreeing_sighting_restarts_the_count() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        let mut moved = device([1; 6], MASTER, 0, 3);
        moved.channel = 12;
        t.observe(&reply(&[moved]), at(base, 1));
        t.observe(&reply(&[moved]), at(base, 2));
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), at(base, 3));
        t.observe(&reply(&[moved]), at(base, 4));
        t.observe(&reply(&[moved]), at(base, 5));
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 8);
        t.observe(&reply(&[moved]), at(base, 6));
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 12);
    }

    #[test]
    fn unheard_devices_go_offline_and_rejoin_at_the_end() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        let a = device([1; 6], MASTER, 0, 3);
        let b = device([2; 6], MASTER, 0, 2);
        t.observe(&reply(&[a, b]), base);
        assert_eq!(t.slot(&[2; 6]), 2);
        t.observe(&reply(&[b]), at(base, 15));
        assert!(t.group(&[1; 6]).unwrap().online(at(base, 15)));
        assert_eq!(t.online().count(), 2);
        t.observe(&reply(&[b]), at(base, 16));
        assert!(!t.group(&[1; 6]).unwrap().online(at(base, 16)));
        assert_eq!(t.online().count(), 1);
        assert_eq!(t.slot(&[2; 6]), 1);
        assert_eq!(t.fans().count(), 1);
        let mut back = a;
        back.channel = 12;
        t.observe(&reply(&[b, back]), at(base, 20));
        let order: Vec<[u8; 6]> = t.online().map(|g| g.device.mac).collect();
        assert_eq!(order, vec![[2; 6], [1; 6]]);
        assert_eq!(t.group(&[1; 6]).unwrap().device.channel, 12);
    }

    #[test]
    fn sweep_marks_offline_without_a_reply_and_forgets_unbound() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(
            &reply(&[device([1; 6], MASTER, 0, 3), device([2; 6], OTHER, 0, 3)]),
            base,
        );
        assert_eq!(t.groups().len(), 2);
        assert_eq!(t.sweep(at(base, 16)), vec![[2; 6]]);
        assert_eq!(t.online().count(), 0);
        assert_eq!(t.groups().len(), 1);
        assert_eq!(t.groups()[0].device.mac, [1; 6]);
    }

    #[test]
    fn a_bound_group_is_forgotten_after_a_long_silence() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        assert!(t
            .sweep(base + FORGET_AFTER - Duration::from_secs(1))
            .is_empty());
        assert_eq!(t.groups().len(), 1);
        assert_eq!(t.sweep(base + FORGET_AFTER), vec![[1; 6]]);
        assert!(t.groups().is_empty());
    }

    #[test]
    fn a_single_odd_record_does_not_change_a_groups_shape() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        let mut odd = device([1; 6], MASTER, 0, 3);
        odd.fan_count = 0;
        odd.fan_types = [20, 20, 20, 0];
        odd.rpm = [900; 4];
        t.observe(&reply(&[odd]), at(base, 1));
        let g = t.group(&[1; 6]).unwrap();
        assert_eq!(g.device.fan_count, 3);
        assert_eq!(g.device.fan_types, [43, 43, 43, 0]);
        assert_eq!(g.device.rpm, [900; 4]);
        assert!(g.has_fans());
        t.observe(&reply(&[odd]), at(base, 2));
        assert_eq!(t.group(&[1; 6]).unwrap().device.fan_count, 3);
        t.observe(&reply(&[odd]), at(base, 3));
        let g = t.group(&[1; 6]).unwrap();
        assert_eq!(g.device.fan_count, 0);
        assert_eq!(g.device.fan_types, [20, 20, 20, 0]);
    }

    #[test]
    fn foreign_and_lighting_devices_take_no_fan_role() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(
            &reply(&[
                device([1; 6], MASTER, 0, 3),
                device([2; 6], OTHER, 0, 3),
                device([3; 6], MASTER, 4, 0),
                device([4; 6], MASTER, 0, 2),
            ]),
            base,
        );
        let fans: Vec<[u8; 6]> = t.fans().map(|g| g.device.mac).collect();
        assert_eq!(fans, vec![[1; 6], [4; 6]]);
        assert_eq!(t.slot(&[1; 6]), 1);
        assert_eq!(t.slot(&[3; 6]), 2);
        assert_eq!(t.slot(&[4; 6]), 3);
        assert_eq!(t.slot(&[2; 6]), 1);
    }

    #[test]
    fn target_is_prepared_and_due_until_sent() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        assert!(t.set_target(&[8; 6], [200; 4]).is_none());
        assert_eq!(
            t.set_target(&[1; 6], [200, 1, 0, 255]),
            Some([200, 25, 0, 0])
        );
        let g = t.group(&[1; 6]).unwrap();
        assert!(g.due(base));
        assert!(!g.acknowledged(base));
        t.sent(&[1; 6], base);
        let g = t.group(&[1; 6]).unwrap();
        assert_eq!(g.unacknowledged, 1);
        assert!(g.due(base));
    }

    #[test]
    fn acknowledged_target_waits_for_the_keepalive() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        t.set_target(&[1; 6], [200, 200, 200, 0]);
        t.sent(&[1; 6], base);
        let mut applied = device([1; 6], MASTER, 0, 3);
        applied.duty = [203, 198, 200, 0];
        t.observe(&reply(&[applied]), base + Duration::from_millis(300));
        let g = t.group(&[1; 6]).unwrap();
        assert!(g.acknowledged(base + Duration::from_millis(300)));
        assert_eq!(g.unacknowledged, 0);
        assert!(!g.due(base + Duration::from_millis(600)));
        assert!(g.due(at(base, 1)));
    }

    #[test]
    fn a_stale_sighting_does_not_acknowledge() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        t.set_target(&[1; 6], [100, 100, 100, 0]);
        t.sent(&[1; 6], base);
        let g = t.group(&[1; 6]).unwrap();
        assert!(g.acknowledged(at(base, 3)));
        assert!(!g.acknowledged(at(base, 4)));
    }

    #[test]
    fn a_new_target_resets_the_send_record() {
        let base = Instant::now();
        let mut t = Tracker::new(MASTER);
        t.observe(&reply(&[device([1; 6], MASTER, 0, 3)]), base);
        t.set_target(&[1; 6], [200; 4]);
        t.sent(&[1; 6], base);
        t.sent(&[1; 6], base);
        assert_eq!(t.set_target(&[1; 6], [200; 4]), Some([200, 200, 200, 0]));
        assert_eq!(t.group(&[1; 6]).unwrap().unacknowledged, 2);
        t.set_target(&[1; 6], [150; 4]);
        let g = t.group(&[1; 6]).unwrap();
        assert_eq!(g.unacknowledged, 0);
        assert!(g.last_sent.is_none());
    }
}
