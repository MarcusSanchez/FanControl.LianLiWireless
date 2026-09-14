//! The 64-byte frames exchanged with the dongles over USB.
//!
//! The transmitter takes a connect request and answers with the master
//! address, and carries radio payloads to the groups in four chunks. The
//! receiver takes a discovery request and answers with one record per
//! device it can hear.

/// Length of every frame written to or read from a dongle.
pub const FRAME_LEN: usize = 64;

/// Length of a radio payload carried to the fan groups.
pub const RF_PAYLOAD_LEN: usize = 240;

/// Payload bytes carried by each frame of a radio transfer.
pub const RF_CHUNK_LEN: usize = 60;

/// Frames needed to carry one radio payload.
pub const RF_CHUNKS: usize = RF_PAYLOAD_LEN / RF_CHUNK_LEN;

/// Receiver type that addresses every group at once.
pub const BROADCAST: u8 = 0xFF;

/// The channel a dongle is most likely to be on.
pub const DEFAULT_CHANNEL: u8 = 8;

/// Most device records a discovery reply page can hold.
pub const RECORDS_PER_PAGE: usize = 10;

/// Bytes a discovery reply page occupies.
pub const PAGE_LEN: usize = 512;

/// Most pages a discovery request can ask for.
pub const MAX_PAGES: u8 = 26;

const SEND_RF: u8 = 0x10;
const GET_MAC: u8 = 0x11;

/// One frame on the wire.
pub type Frame = [u8; FRAME_LEN];

/// One radio payload before chunking.
pub type RfPayload = [u8; RF_PAYLOAD_LEN];

/// What the transmitter answers to a connect request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectReply {
    /// Address of the dongle itself, the master the groups are bound to.
    pub master_mac: [u8; 6],
    /// Firmware version of the transmitter.
    pub firmware: u16,
}

/// The connect request for one channel, written to the transmitter.
pub fn connect_request(channel: u8) -> Frame {
    let mut frame = [0; FRAME_LEN];
    frame[0] = GET_MAC;
    frame[1] = channel;
    frame
}

/// Reads the transmitter's answer to a connect request.
///
/// Returns `None` for a short reply, a wrong command byte, an all-zero
/// address, or a status word that says the dongle is not on that channel.
pub fn parse_connect_reply(reply: &[u8]) -> Option<ConnectReply> {
    if reply.len() < 13 || reply[0] != GET_MAC {
        return None;
    }
    let mut master_mac = [0; 6];
    master_mac.copy_from_slice(&reply[1..7]);
    if master_mac == [0; 6] {
        return None;
    }
    let status = u32::from_be_bytes([reply[7], reply[8], reply[9], reply[10]]);
    if status <= 1 {
        return None;
    }
    Some(ConnectReply {
        master_mac,
        firmware: u16::from_be_bytes([reply[11], reply[12]]),
    })
}

/// The channels to try when connecting, most likely first: the default,
/// then the even channels, then the odd ones.
pub fn channel_scan() -> impl Iterator<Item = u8> {
    std::iter::once(DEFAULT_CHANNEL)
        .chain((2..=38).filter(|c| c % 2 == 0 && *c != DEFAULT_CHANNEL))
        .chain((1..=39).filter(|c| c % 2 == 1))
}

/// How many times to try a channel before moving on.
pub fn connect_attempts(channel: u8) -> u8 {
    if channel == DEFAULT_CHANNEL {
        3
    } else {
        1
    }
}

/// The discovery request, written to the receiver.
///
/// `pages` is how many reply pages to ask for and is held to `1..=MAX_PAGES`.
/// Bytes 2 and 3 carry a fan speed for receivers that mirror one on their
/// PWM output; they stay zero here.
pub fn discovery_request(pages: u8) -> Frame {
    let mut frame = [0; FRAME_LEN];
    frame[0] = SEND_RF;
    frame[1] = pages.clamp(1, MAX_PAGES);
    frame
}

/// How many bytes a discovery reply of `pages` pages can occupy.
pub fn discovery_reply_len(pages: u8) -> usize {
    usize::from(pages.clamp(1, MAX_PAGES)) * PAGE_LEN
}

/// How many pages the receiver needs to report `devices` devices.
pub fn pages_for(devices: u8) -> u8 {
    (usize::from(devices).div_ceil(RECORDS_PER_PAGE) as u8).max(1)
}

/// Splits a radio payload into the frames that carry it to the transmitter.
///
/// Each frame names its chunk index, the channel, and the receiver type of
/// the group addressed, or [`BROADCAST`] for every group.
pub fn rf_frames(channel: u8, receiver: u8, payload: &RfPayload) -> [Frame; RF_CHUNKS] {
    let mut frames = [[0; FRAME_LEN]; RF_CHUNKS];
    for (index, frame) in frames.iter_mut().enumerate() {
        frame[0] = SEND_RF;
        frame[1] = index as u8;
        frame[2] = channel;
        frame[3] = receiver;
        let start = index * RF_CHUNK_LEN;
        frame[4..].copy_from_slice(&payload[start..start + RF_CHUNK_LEN]);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0xbf, 0x3d, 0xca, 0xe5, 0x66, 0xe4];

    fn good_reply() -> [u8; 64] {
        let mut reply = [0; 64];
        reply[0] = 0x11;
        reply[1..7].copy_from_slice(&MAC);
        reply[7..11].copy_from_slice(&[0, 0, 0, 2]);
        reply[11..13].copy_from_slice(&[1, 6]);
        reply
    }

    #[test]
    fn connect_request_names_the_channel() {
        let frame = connect_request(8);
        assert_eq!(&frame[..2], &[0x11, 8]);
        assert!(frame[2..].iter().all(|b| *b == 0));
        assert_eq!(frame.len(), 64);
    }

    #[test]
    fn connect_reply_yields_master_and_firmware() {
        let reply = parse_connect_reply(&good_reply()).unwrap();
        assert_eq!(reply.master_mac, MAC);
        assert_eq!(reply.firmware, 0x0106);
    }

    #[test]
    fn connect_reply_accepts_exactly_thirteen_bytes() {
        assert!(parse_connect_reply(&good_reply()[..13]).is_some());
        assert!(parse_connect_reply(&good_reply()[..12]).is_none());
    }

    #[test]
    fn connect_reply_rejects_wrong_command() {
        let mut reply = good_reply();
        reply[0] = 0x10;
        assert!(parse_connect_reply(&reply).is_none());
    }

    #[test]
    fn connect_reply_rejects_zero_address() {
        let mut reply = good_reply();
        reply[1..7].fill(0);
        assert!(parse_connect_reply(&reply).is_none());
    }

    #[test]
    fn connect_reply_rejects_low_status() {
        for status in [0u32, 1] {
            let mut reply = good_reply();
            reply[7..11].copy_from_slice(&status.to_be_bytes());
            assert!(parse_connect_reply(&reply).is_none());
        }
        let mut reply = good_reply();
        reply[7..11].copy_from_slice(&0x0100_0000u32.to_be_bytes());
        assert!(parse_connect_reply(&reply).is_some());
    }

    #[test]
    fn channel_scan_is_default_then_even_then_odd() {
        let order: Vec<u8> = channel_scan().collect();
        assert_eq!(order.len(), 39);
        assert_eq!(order[0], 8);
        assert_eq!(&order[1..5], &[2, 4, 6, 10]);
        assert_eq!(order[18], 38);
        assert_eq!(&order[19..22], &[1, 3, 5]);
        assert_eq!(order[38], 39);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, (1..=39).collect::<Vec<u8>>());
    }

    #[test]
    fn default_channel_gets_three_attempts() {
        assert_eq!(connect_attempts(8), 3);
        assert_eq!(connect_attempts(2), 1);
        assert_eq!(connect_attempts(39), 1);
    }

    #[test]
    fn discovery_request_names_the_page_count() {
        let frame = discovery_request(1);
        assert_eq!(&frame[..4], &[0x10, 1, 0, 0]);
        assert!(frame[4..].iter().all(|b| *b == 0));
        assert_eq!(discovery_request(0)[1], 1);
        assert_eq!(discovery_request(3)[1], 3);
        assert_eq!(discovery_request(200)[1], 26);
    }

    #[test]
    fn discovery_reply_len_is_pages_of_512() {
        assert_eq!(discovery_reply_len(1), 512);
        assert_eq!(discovery_reply_len(0), 512);
        assert_eq!(discovery_reply_len(3), 1536);
        assert_eq!(discovery_reply_len(26), 26 * 512);
        assert_eq!(discovery_reply_len(255), 26 * 512);
    }

    #[test]
    fn pages_for_rounds_devices_up() {
        assert_eq!(pages_for(0), 1);
        assert_eq!(pages_for(1), 1);
        assert_eq!(pages_for(10), 1);
        assert_eq!(pages_for(11), 2);
        assert_eq!(pages_for(255), 26);
    }

    #[test]
    fn rf_frames_carry_the_payload_in_four_chunks() {
        let mut payload = [0; RF_PAYLOAD_LEN];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = i as u8;
        }
        let frames = rf_frames(8, 2, &payload);
        assert_eq!(frames.len(), 4);
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(&frame[..4], &[0x10, index as u8, 8, 2]);
            assert_eq!(&frame[4..], &payload[index * 60..index * 60 + 60]);
        }
        assert_eq!(frames[3][63], 239);
    }

    #[test]
    fn rf_frames_can_broadcast() {
        let frames = rf_frames(8, BROADCAST, &[0; RF_PAYLOAD_LEN]);
        assert!(frames.iter().all(|f| f[3] == 0xFF));
    }
}
