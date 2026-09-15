//! The dongle pair as one thing: connect to the transmitter, poll the
//! receiver, send radio payloads.
//!
//! The sequences are written over a small port trait so they can be
//! exercised against scripted devices as well as real ones.

use crate::device::{self, Device, TIMEOUT};
use crate::discovery::{self, Reply};
use crate::enumerate::{self, Role};
use crate::frame::{self, ConnectReply, Frame, RfPayload, FRAME_LEN};
use crate::win::WinError;
use std::fmt;
use std::thread;
use std::time::Duration;

/// How long to wait for the transmitter's answer to a connect request.
pub const CONNECT_WAIT: Duration = Duration::from_millis(500);

/// Tries on the known channel when reconnecting.
pub const RECONNECT_ATTEMPTS: u8 = 3;

/// How long to wait for the first piece of a discovery reply.
pub const REPLY_WAIT: Duration = Duration::from_millis(100);

/// How long a discovery reply may pause before it is taken as finished.
pub const REPLY_PAUSE: Duration = Duration::from_millis(10);

/// How long a flush waits for each stale frame.
pub const FLUSH_WAIT: Duration = Duration::from_millis(5);

/// Gap between the frames of one radio payload.
pub const CHUNK_GAP: Duration = Duration::from_millis(1);

/// Something frames can be written to and read from.
pub trait Port {
    /// Writes one frame, returning how many bytes went out.
    fn write(&mut self, frame: &[u8], timeout: Duration) -> Result<usize, WinError>;
    /// Reads what is waiting, returning 0 when nothing arrives in time.
    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, WinError>;
}

impl Port for Device {
    fn write(&mut self, frame: &[u8], timeout: Duration) -> Result<usize, WinError> {
        Device::write(self, frame, timeout)
    }

    fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, WinError> {
        Device::read(self, buffer, timeout)
    }
}

/// Why the dongle could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The dongles are not both attached.
    Find(enumerate::Error),
    /// One dongle would not open.
    Open(Role, device::Error),
    /// A transfer failed.
    Transfer(Role, WinError),
    /// No channel produced a valid connect reply.
    NoMaster,
    /// A frame went out short.
    ShortWrite(Role, usize),
    /// The receiver's reply could not be read.
    Reply(discovery::Error),
    /// The handles were given up for a reopen that then failed.
    Closed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Find(error) => error.fmt(f),
            Self::Open(role, error) => write!(f, "opening the {role}: {error}"),
            Self::Transfer(role, error) => write!(f, "{role}: {error}"),
            Self::NoMaster => f.write_str("the transmitter answered on no channel"),
            Self::ShortWrite(role, n) => write!(f, "{role} took {n} of {FRAME_LEN} bytes"),
            Self::Reply(error) => error.fmt(f),
            Self::Closed => f.write_str("the dongle is closed until it is reopened"),
        }
    }
}

impl std::error::Error for Error {}

/// Discards frames the port has waiting, up to `max_reads` of them.
pub fn drain<P: Port>(port: &mut P, chunk_len: usize, max_reads: usize) -> Result<(), WinError> {
    let mut stale = vec![0u8; chunk_len];
    for _ in 0..max_reads {
        if port.read(&mut stale, FLUSH_WAIT)? == 0 {
            break;
        }
    }
    Ok(())
}

/// Reads a reply that arrives in pieces: waits `first` for the opening
/// piece, then keeps reading until `pause` passes with nothing more or
/// the buffer is full. Returns how much arrived.
pub fn gather<P: Port>(
    port: &mut P,
    buffer: &mut [u8],
    first: Duration,
    pause: Duration,
) -> Result<usize, WinError> {
    let mut total = 0;
    let mut timeout = first;
    let mut chunk = [0u8; FRAME_LEN];
    while total < buffer.len() {
        match port.read(&mut chunk, timeout)? {
            0 => break,
            n => {
                let n = n.min(chunk.len()).min(buffer.len() - total);
                buffer[total..total + n].copy_from_slice(&chunk[..n]);
                total += n;
                timeout = pause;
            }
        }
    }
    Ok(total)
}

fn write_frame<P: Port>(port: &mut P, role: Role, frame: &Frame) -> Result<(), Error> {
    let n = port
        .write(frame, TIMEOUT)
        .map_err(|e| Error::Transfer(role, e))?;
    if n != FRAME_LEN {
        return Err(Error::ShortWrite(role, n));
    }
    Ok(())
}

/// Asks the transmitter which dongle it is and on which channel, trying
/// the channels in the usual order.
///
/// Silence or a malformed answer moves on to the next attempt; a failed
/// transfer ends the scan at once, since the transmitter is not going to
/// answer on another channel either.
pub fn connect<P: Port>(tx: &mut P) -> Result<(ConnectReply, u8), Error> {
    connect_with(
        tx,
        frame::channel_scan().map(|c| (c, frame::connect_attempts(c))),
    )
}

/// Asks the transmitter which dongle it is on one channel only, trying
/// [`RECONNECT_ATTEMPTS`] times. Bounded to a second and a half, for a
/// reconnect that must not hold up the loop.
pub fn connect_on<P: Port>(tx: &mut P, channel: u8) -> Result<(ConnectReply, u8), Error> {
    connect_with(tx, std::iter::once((channel, RECONNECT_ATTEMPTS)))
}

fn connect_with<P: Port>(
    tx: &mut P,
    plan: impl Iterator<Item = (u8, u8)>,
) -> Result<(ConnectReply, u8), Error> {
    let role = Role::Transmitter;
    for (channel, attempts) in plan {
        for _ in 0..attempts {
            drain(tx, FRAME_LEN, 16).map_err(|e| Error::Transfer(role, e))?;
            write_frame(tx, role, &frame::connect_request(channel))?;
            let mut reply = [0u8; FRAME_LEN];
            let n = tx
                .read(&mut reply, CONNECT_WAIT)
                .map_err(|e| Error::Transfer(role, e))?;
            if let Some(parsed) = frame::parse_connect_reply(&reply[..n]) {
                return Ok((parsed, channel));
            }
        }
    }
    Err(Error::NoMaster)
}

/// Asks the receiver for its device list, `pages` pages of it.
pub fn poll<P: Port>(rx: &mut P, pages: u8) -> Result<Reply, Error> {
    let role = Role::Receiver;
    drain(rx, 512, 64).map_err(|e| Error::Transfer(role, e))?;
    write_frame(rx, role, &frame::discovery_request(pages))?;
    let mut buffer = vec![0u8; frame::discovery_reply_len(pages)];
    let n =
        gather(rx, &mut buffer, REPLY_WAIT, REPLY_PAUSE).map_err(|e| Error::Transfer(role, e))?;
    discovery::parse_reply(&buffer[..n], pages).map_err(Error::Reply)
}

/// Sends one radio payload through the transmitter to a receiver type on
/// a channel, as four frames.
pub fn send<P: Port>(
    tx: &mut P,
    channel: u8,
    receiver: u8,
    payload: &RfPayload,
) -> Result<(), Error> {
    for chunk in frame::rf_frames(channel, receiver, payload) {
        write_frame(tx, Role::Transmitter, &chunk)?;
        thread::sleep(CHUNK_GAP);
    }
    Ok(())
}

/// Both dongles, open and connected.
pub struct Dongle {
    handles: Option<Handles>,
    /// What the transmitter said about itself.
    pub master: ConnectReply,
    /// The channel it answered on.
    pub channel: u8,
    pages: u8,
}

struct Handles {
    tx: Device,
    rx: Device,
}

impl Dongle {
    /// Finds, opens and connects to the dongle pair, scanning every
    /// channel for the transmitter.
    pub fn open() -> Result<Self, Error> {
        Self::open_with(None)
    }

    fn open_with(channel: Option<u8>) -> Result<Self, Error> {
        let paths = enumerate::find().map_err(Error::Find)?;
        let mut tx =
            Device::open(&paths.transmitter).map_err(|e| Error::Open(Role::Transmitter, e))?;
        let rx = Device::open(&paths.receiver).map_err(|e| Error::Open(Role::Receiver, e))?;
        let (master, channel) = match channel {
            Some(channel) => connect_on(&mut tx, channel)?,
            None => connect(&mut tx)?,
        };
        Ok(Self {
            handles: Some(Handles { tx, rx }),
            master,
            channel,
            pages: 1,
        })
    }

    /// Closes the dongle pair, then finds, opens and connects it again,
    /// on the channel it was on unless `every_channel` asks for the full
    /// scan. WinUSB gives an interface to one handle at a time, so the
    /// old handles go first; if the reopen fails, every transfer reports
    /// [`Error::Closed`] until a later reopen succeeds.
    pub fn reopen(&mut self, every_channel: bool) -> Result<(), Error> {
        let channel = self.channel;
        self.handles = None;
        let fresh = Self::open_with(if every_channel { None } else { Some(channel) })?;
        *self = fresh;
        Ok(())
    }

    /// One discovery poll. The page count follows what the receiver last
    /// reported.
    pub fn poll(&mut self) -> Result<Reply, Error> {
        let handles = self.handles.as_mut().ok_or(Error::Closed)?;
        let reply = poll(&mut handles.rx, self.pages)?;
        self.pages = frame::pages_for(reply.reported);
        Ok(reply)
    }

    /// Sends one radio payload to a receiver type on the dongle's channel.
    pub fn send(&mut self, receiver: u8, payload: &RfPayload) -> Result<(), Error> {
        self.send_on(self.channel, receiver, payload)
    }

    /// Sends one radio payload to a receiver type on a given channel, the
    /// one the device itself reported.
    pub fn send_on(&mut self, channel: u8, receiver: u8, payload: &RfPayload) -> Result<(), Error> {
        let handles = self.handles.as_mut().ok_or(Error::Closed)?;
        send(&mut handles.tx, channel, receiver, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::ERROR_DEVICE_NOT_CONNECTED;
    use std::collections::VecDeque;

    type Reads = Vec<Result<Vec<u8>, WinError>>;

    /// A scripted port. Reads serve what is waiting; a write records the
    /// frame and queues the reads scripted for that write. An exhausted
    /// queue reads as silence.
    struct Fake {
        waiting: VecDeque<Result<Vec<u8>, WinError>>,
        per_write: VecDeque<Reads>,
        writes: Vec<Vec<u8>>,
        timeouts: Vec<Duration>,
    }

    impl Fake {
        fn new(waiting: Reads) -> Self {
            Self {
                waiting: waiting.into(),
                per_write: VecDeque::new(),
                writes: Vec::new(),
                timeouts: Vec::new(),
            }
        }

        fn answering(per_write: Vec<Reads>) -> Self {
            let mut fake = Self::new(Vec::new());
            fake.per_write = per_write.into();
            fake
        }
    }

    impl Port for Fake {
        fn write(&mut self, frame: &[u8], _: Duration) -> Result<usize, WinError> {
            self.writes.push(frame.to_vec());
            self.waiting = self.per_write.pop_front().unwrap_or_default().into();
            Ok(frame.len())
        }

        fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, WinError> {
            self.timeouts.push(timeout);
            match self.waiting.pop_front() {
                None => Ok(0),
                Some(Err(e)) => Err(e),
                Some(Ok(bytes)) => {
                    let n = bytes.len().min(buffer.len());
                    buffer[..n].copy_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
    }

    const MAC: [u8; 6] = [0xbf, 0x3d, 0xca, 0xe5, 0x66, 0xe4];

    fn connect_reply() -> Vec<u8> {
        let mut reply = vec![0; 64];
        reply[0] = 0x11;
        reply[1..7].copy_from_slice(&MAC);
        reply[7..11].copy_from_slice(&[0, 0, 0, 2]);
        reply[11..13].copy_from_slice(&[1, 6]);
        reply
    }

    #[test]
    fn gather_returns_nothing_when_the_first_piece_never_comes() {
        let mut port = Fake::new(vec![]);
        assert_eq!(
            gather(&mut port, &mut [0; 512], TIMEOUT, TIMEOUT).unwrap(),
            0
        );
    }

    #[test]
    fn gather_keeps_what_arrived_before_the_pause() {
        let mut port = Fake::new(vec![Ok(vec![0x10, 0, 0x80])]);
        let mut buffer = [0; 512];
        let n = gather(&mut port, &mut buffer, REPLY_WAIT, REPLY_PAUSE).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buffer[..3], &[0x10, 0, 0x80]);
        assert_eq!(port.timeouts, vec![REPLY_WAIT, REPLY_PAUSE]);
    }

    #[test]
    fn gather_passes_on_a_failure_after_a_partial_reply() {
        let lost = WinError {
            call: "WinUsb_ReadPipe",
            code: ERROR_DEVICE_NOT_CONNECTED,
        };
        let mut port = Fake::new(vec![Ok(vec![0x10; 64]), Err(lost)]);
        assert_eq!(
            gather(&mut port, &mut [0; 512], TIMEOUT, TIMEOUT),
            Err(lost)
        );
    }

    #[test]
    fn gather_stops_at_capacity_and_clips() {
        let mut port = Fake::new(vec![Ok(vec![9; 64]), Ok(vec![9; 64])]);
        let mut buffer = [0; 100];
        assert_eq!(
            gather(&mut port, &mut buffer, TIMEOUT, TIMEOUT).unwrap(),
            100
        );
        assert_eq!(buffer, [9; 100]);
        assert_eq!(port.timeouts.len(), 2);
        let mut port = Fake::new(vec![Ok(vec![1])]);
        assert_eq!(gather(&mut port, &mut [], TIMEOUT, TIMEOUT).unwrap(), 0);
        assert!(port.timeouts.is_empty());
    }

    #[test]
    fn drain_reads_until_silence_or_the_cap() {
        let mut port = Fake::new(vec![Ok(vec![1; 64]), Ok(vec![2; 64])]);
        drain(&mut port, 64, 16).unwrap();
        assert_eq!(port.timeouts.len(), 3);
        let mut port = Fake::new((0..100).map(|_| Ok(vec![1; 64])).collect());
        drain(&mut port, 64, 16).unwrap();
        assert_eq!(port.timeouts.len(), 16);
    }

    #[test]
    fn connect_takes_the_first_valid_reply() {
        let mut port = Fake::answering(vec![vec![Ok(connect_reply())]]);
        let (reply, channel) = connect(&mut port).unwrap();
        assert_eq!(reply.master_mac, MAC);
        assert_eq!(reply.firmware, 0x0106);
        assert_eq!(channel, 8);
        assert_eq!(port.writes.len(), 1);
        assert_eq!(&port.writes[0][..2], &[0x11, 8]);
        assert_eq!(port.timeouts.len(), 2);
        assert_eq!(port.timeouts[1], CONNECT_WAIT);
    }

    #[test]
    fn connect_moves_on_after_bad_replies_and_gives_up_after_the_scan() {
        let mut bad = connect_reply();
        bad[0] = 0x10;
        let mut port = Fake::answering(vec![vec![Ok(bad.clone())], vec![], vec![Ok(bad)]]);
        assert_eq!(connect(&mut port), Err(Error::NoMaster));
        let channels: Vec<u8> = port.writes.iter().map(|w| w[1]).collect();
        assert_eq!(channels.len(), 41);
        assert_eq!(&channels[..4], &[8, 8, 8, 2]);
        assert_eq!(channels[40], 39);
    }

    #[test]
    fn connect_stops_at_the_first_failed_transfer() {
        let lost = WinError {
            call: "WinUsb_ReadPipe",
            code: ERROR_DEVICE_NOT_CONNECTED,
        };
        let mut port = Fake::answering(vec![vec![], vec![Err(lost)]]);
        assert_eq!(
            connect(&mut port),
            Err(Error::Transfer(Role::Transmitter, lost))
        );
        assert_eq!(port.writes.len(), 2);

        struct Refusing;
        impl Port for Refusing {
            fn write(&mut self, _: &[u8], _: Duration) -> Result<usize, WinError> {
                Err(WinError {
                    call: "WinUsb_WritePipe",
                    code: ERROR_DEVICE_NOT_CONNECTED,
                })
            }
            fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, WinError> {
                Ok(0)
            }
        }
        assert!(matches!(
            connect(&mut Refusing),
            Err(Error::Transfer(Role::Transmitter, _))
        ));
    }

    #[test]
    fn connect_on_tries_one_channel_a_few_times() {
        let mut port = Fake::answering(vec![vec![], vec![Ok(connect_reply())]]);
        let (_, channel) = connect_on(&mut port, 8).unwrap();
        assert_eq!(channel, 8);
        assert_eq!(port.writes.len(), 2);

        let mut silent = Fake::answering(vec![]);
        assert_eq!(connect_on(&mut silent, 12), Err(Error::NoMaster));
        let channels: Vec<u8> = silent.writes.iter().map(|w| w[1]).collect();
        assert_eq!(channels, vec![12; usize::from(RECONNECT_ATTEMPTS)]);
    }

    #[test]
    fn connect_answers_on_a_later_channel() {
        let mut port = Fake::answering(vec![vec![], vec![], vec![], vec![Ok(connect_reply())]]);
        let (_, channel) = connect(&mut port).unwrap();
        assert_eq!(channel, 2);
        assert_eq!(port.writes.len(), 4);
    }

    #[test]
    fn poll_drains_asks_and_parses() {
        let mut record = vec![0u8; 42];
        record[0..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        record[6..12].copy_from_slice(&MAC);
        record[12] = 8;
        record[13] = 2;
        record[19] = 3;
        record[41] = 0x1C;
        let mut reply = vec![0x10, 1, 0, 0];
        reply.extend_from_slice(&record);
        let mut port = Fake::new(vec![Ok(vec![0xAA; 512])]);
        port.per_write = vec![vec![Ok(reply[..40].to_vec()), Ok(reply[40..].to_vec())]].into();
        let parsed = poll(&mut port, 1).unwrap();
        assert_eq!(parsed.reported, 1);
        assert_eq!(parsed.devices.len(), 1);
        assert_eq!(parsed.devices[0].mac, [1, 2, 3, 4, 5, 6]);
        assert_eq!(port.writes.len(), 1);
        assert_eq!(&port.writes[0][..2], &[0x10, 1]);
        assert_eq!(port.timeouts[..2], [FLUSH_WAIT, FLUSH_WAIT]);
        assert_eq!(port.timeouts[2], REPLY_WAIT);
        assert_eq!(port.timeouts[3], REPLY_PAUSE);
    }

    #[test]
    fn poll_reports_a_bad_reply() {
        let mut port = Fake::answering(vec![vec![Ok(vec![0x11, 0, 0, 0])]]);
        assert_eq!(
            poll(&mut port, 1),
            Err(Error::Reply(discovery::Error::WrongCommand(0x11)))
        );
        let mut port = Fake::answering(vec![vec![]]);
        assert_eq!(
            poll(&mut port, 1),
            Err(Error::Reply(discovery::Error::Short))
        );
    }

    #[test]
    fn send_writes_four_frames() {
        let mut port = Fake::new(vec![]);
        let mut payload = [0u8; 240];
        payload[0] = 0x12;
        send(&mut port, 8, 0xFF, &payload).unwrap();
        assert_eq!(port.writes.len(), 4);
        assert_eq!(&port.writes[0][..5], &[0x10, 0, 8, 0xFF, 0x12]);
        assert_eq!(&port.writes[3][..4], &[0x10, 3, 8, 0xFF]);
    }

    #[test]
    fn a_short_write_is_an_error() {
        struct Short;
        impl Port for Short {
            fn write(&mut self, _: &[u8], _: Duration) -> Result<usize, WinError> {
                Ok(10)
            }
            fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, WinError> {
                Ok(0)
            }
        }
        assert_eq!(
            poll(&mut Short, 1),
            Err(Error::ShortWrite(Role::Receiver, 10))
        );
        assert_eq!(
            Error::ShortWrite(Role::Receiver, 10).to_string(),
            "receiver took 10 of 64 bytes"
        );
    }
}
