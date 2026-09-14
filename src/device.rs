//! One open dongle: its file handle, its WinUSB interface, and the two
//! pipes it talks through.
//!
//! Both dongles present one interface with an OUT pipe at `0x01` and an
//! IN pipe at `0x81`. Whether a pipe is bulk or interrupt is read from the
//! descriptor rather than assumed.

use crate::win::*;
use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::time::Duration;

/// The pipe frames are written to.
pub const OUT_PIPE: u8 = 0x01;

/// The pipe replies are read from.
pub const IN_PIPE: u8 = 0x81;

/// Timeout for ordinary transfers.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// How long a flush waits for each stale frame.
pub const FLUSH_WAIT: Duration = Duration::from_millis(5);

/// What kind of transfer a pipe carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// Bulk transfers.
    Bulk,
    /// Interrupt transfers, polled at the pipe's interval.
    Interrupt,
}

/// One endpoint as the descriptor describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipe {
    /// Endpoint address, with the direction bit.
    pub id: u8,
    /// Bulk or interrupt.
    pub transfer: Transfer,
    /// Largest packet the endpoint moves at once.
    pub max_packet: u16,
}

/// Why a device could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A Windows call failed.
    Windows(WinError),
    /// The interface has no pipe with this address.
    NoPipe(u8),
    /// A pipe is neither bulk nor interrupt.
    PipeType(u8, i32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => error.fmt(f),
            Self::NoPipe(id) => write!(f, "the device has no pipe 0x{id:02x}"),
            Self::PipeType(id, kind) => {
                write!(f, "pipe 0x{id:02x} has unusable transfer type {kind}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<WinError> for Error {
    fn from(error: WinError) -> Self {
        Self::Windows(error)
    }
}

/// An open dongle. Closing happens on drop.
pub struct Device {
    winusb: &'static WinUsb,
    file: HANDLE,
    interface: WINUSB_INTERFACE_HANDLE,
    out_pipe: Pipe,
    in_pipe: Pipe,
    read_timeout: Option<u32>,
    write_timeout: Option<u32>,
}

unsafe impl Send for Device {}

impl Device {
    /// Opens the device at a path from enumeration.
    pub fn open(path: &str) -> Result<Self, Error> {
        let winusb = WinUsb::get()?;
        let file = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if file == INVALID_HANDLE_VALUE {
            return Err(WinError::last("CreateFileW").into());
        }
        let mut interface: WINUSB_INTERFACE_HANDLE = ptr::null_mut();
        if unsafe { (winusb.Initialize)(file, &mut interface) } == 0 {
            let error = WinError::last("WinUsb_Initialize");
            unsafe {
                CloseHandle(file);
            }
            return Err(error.into());
        }
        let mut device = Self {
            winusb,
            file,
            interface,
            out_pipe: Pipe {
                id: OUT_PIPE,
                transfer: Transfer::Bulk,
                max_packet: 0,
            },
            in_pipe: Pipe {
                id: IN_PIPE,
                transfer: Transfer::Bulk,
                max_packet: 0,
            },
            read_timeout: None,
            write_timeout: None,
        };
        let pipes = device.pipes()?;
        device.out_pipe = *pipes
            .iter()
            .find(|p| p.id == OUT_PIPE)
            .ok_or(Error::NoPipe(OUT_PIPE))?;
        device.in_pipe = *pipes
            .iter()
            .find(|p| p.id == IN_PIPE)
            .ok_or(Error::NoPipe(IN_PIPE))?;
        device.set_policy(IN_PIPE, AUTO_CLEAR_STALL, 1u8)?;
        device.set_policy(IN_PIPE, ALLOW_PARTIAL_READS, 1u8)?;
        Ok(device)
    }

    /// The pipe frames go out on.
    pub fn out_pipe(&self) -> Pipe {
        self.out_pipe
    }

    /// The pipe replies come in on.
    pub fn in_pipe(&self) -> Pipe {
        self.in_pipe
    }

    fn pipes(&self) -> Result<Vec<Pipe>, Error> {
        let mut descriptor = USB_INTERFACE_DESCRIPTOR::default();
        if unsafe { (self.winusb.QueryInterfaceSettings)(self.interface, 0, &mut descriptor) } == 0
        {
            return Err(WinError::last("WinUsb_QueryInterfaceSettings").into());
        }
        let mut pipes = Vec::new();
        for index in 0..descriptor.bNumEndpoints {
            let mut info = WINUSB_PIPE_INFORMATION::default();
            if unsafe { (self.winusb.QueryPipe)(self.interface, 0, index, &mut info) } == 0 {
                return Err(WinError::last("WinUsb_QueryPipe").into());
            }
            let transfer = match info.PipeType {
                USBD_PIPE_TYPE_BULK => Transfer::Bulk,
                USBD_PIPE_TYPE_INTERRUPT => Transfer::Interrupt,
                other => return Err(Error::PipeType(info.PipeId, other)),
            };
            pipes.push(Pipe {
                id: info.PipeId,
                transfer,
                max_packet: info.MaximumPacketSize,
            });
        }
        Ok(pipes)
    }

    fn set_policy<T>(&self, pipe: u8, policy: DWORD, value: T) -> Result<(), WinError> {
        let ok = unsafe {
            (self.winusb.SetPipePolicy)(
                self.interface,
                pipe,
                policy,
                std::mem::size_of::<T>() as DWORD,
                &value as *const T as *const c_void,
            )
        };
        if ok == 0 {
            return Err(WinError::last("WinUsb_SetPipePolicy"));
        }
        Ok(())
    }

    fn apply_timeout(&mut self, pipe: u8, timeout: Duration) -> Result<(), WinError> {
        let millis = timeout_millis(timeout);
        let cached = if pipe == IN_PIPE {
            &mut self.read_timeout
        } else {
            &mut self.write_timeout
        };
        if *cached == Some(millis) {
            return Ok(());
        }
        let winusb = self.winusb;
        let ok = unsafe {
            (winusb.SetPipePolicy)(
                self.interface,
                pipe,
                PIPE_TRANSFER_TIMEOUT,
                std::mem::size_of::<u32>() as DWORD,
                &millis as *const u32 as *const c_void,
            )
        };
        if ok == 0 {
            return Err(WinError::last("WinUsb_SetPipePolicy"));
        }
        *cached = Some(millis);
        Ok(())
    }

    /// Writes one frame, returning how many bytes went out.
    pub fn write(&mut self, frame: &[u8], timeout: Duration) -> Result<usize, WinError> {
        self.apply_timeout(OUT_PIPE, timeout)?;
        let mut written: DWORD = 0;
        let ok = unsafe {
            (self.winusb.WritePipe)(
                self.interface,
                OUT_PIPE,
                frame.as_ptr(),
                frame.len() as DWORD,
                &mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(WinError::last("WinUsb_WritePipe"));
        }
        Ok(written as usize)
    }

    /// Reads what the device has to say, up to the buffer's length.
    ///
    /// Returns 0 when nothing arrived within `timeout`.
    pub fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, WinError> {
        self.apply_timeout(IN_PIPE, timeout)?;
        let mut read: DWORD = 0;
        let ok = unsafe {
            (self.winusb.ReadPipe)(
                self.interface,
                IN_PIPE,
                buffer.as_mut_ptr(),
                buffer.len() as DWORD,
                &mut read,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = WinError::last("WinUsb_ReadPipe");
            if error.code == ERROR_SEM_TIMEOUT {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(read as usize)
    }

    /// Discards whatever the device sent that nobody read.
    pub fn flush(&mut self) {
        let mut stale = [0u8; 512];
        for _ in 0..64 {
            match self.read(&mut stale, FLUSH_WAIT) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    /// Reads a reply that arrives in pieces: waits `first` for the opening
    /// piece, then keeps reading until `pause` passes with nothing more or
    /// the buffer is full. Returns how much arrived.
    pub fn read_until_silence(
        &mut self,
        buffer: &mut [u8],
        first: Duration,
        pause: Duration,
    ) -> Result<usize, WinError> {
        gather(buffer, first, pause, |chunk, timeout| self.read(chunk, timeout))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            (self.winusb.Free)(self.interface);
            CloseHandle(self.file);
        }
    }
}

fn timeout_millis(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

fn gather(
    buffer: &mut [u8],
    first: Duration,
    pause: Duration,
    mut read: impl FnMut(&mut [u8], Duration) -> Result<usize, WinError>,
) -> Result<usize, WinError> {
    let mut total = 0;
    let mut timeout = first;
    let mut chunk = [0u8; 64];
    while total < buffer.len() {
        match read(&mut chunk, timeout)? {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_become_whole_milliseconds_of_at_least_one() {
        assert_eq!(timeout_millis(Duration::from_millis(5)), 5);
        assert_eq!(timeout_millis(Duration::from_micros(200)), 1);
        assert_eq!(timeout_millis(Duration::ZERO), 1);
        assert_eq!(timeout_millis(Duration::from_secs(5)), 5000);
        assert_eq!(timeout_millis(Duration::from_secs(1 << 40)), u32::MAX);
    }

    #[test]
    fn gather_returns_nothing_when_the_first_piece_never_comes() {
        let n = gather(&mut [0; 512], TIMEOUT, TIMEOUT, |_, _| Ok(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn gather_keeps_what_arrived_before_the_pause() {
        let first = Duration::from_millis(100);
        let pause = Duration::from_millis(10);
        let mut calls = 0;
        let mut buffer = [0; 512];
        let n = gather(&mut buffer, first, pause, |chunk, timeout| {
            calls += 1;
            if calls == 1 {
                assert_eq!(timeout, first);
                chunk[..3].copy_from_slice(&[0x10, 0, 0x80]);
                Ok(3)
            } else {
                assert_eq!(timeout, pause);
                Ok(0)
            }
        })
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buffer[..3], &[0x10, 0, 0x80]);
        assert_eq!(calls, 2);
    }

    #[test]
    fn gather_passes_on_a_failure_after_a_partial_reply() {
        let mut calls = 0;
        let result = gather(&mut [0; 512], TIMEOUT, TIMEOUT, |chunk, _| {
            calls += 1;
            if calls == 1 {
                chunk.fill(0x10);
                Ok(64)
            } else {
                Err(WinError {
                    call: "WinUsb_ReadPipe",
                    code: ERROR_DEVICE_NOT_CONNECTED,
                })
            }
        });
        assert_eq!(
            result,
            Err(WinError {
                call: "WinUsb_ReadPipe",
                code: ERROR_DEVICE_NOT_CONNECTED
            })
        );
    }

    #[test]
    fn gather_stops_at_capacity_and_skips_an_empty_buffer() {
        let mut calls = 0;
        let mut buffer = [0; 64];
        let n = gather(&mut buffer, TIMEOUT, TIMEOUT, |chunk, _| {
            calls += 1;
            chunk.fill(9);
            Ok(64)
        })
        .unwrap();
        assert_eq!(n, 64);
        assert_eq!(calls, 1);
        assert_eq!(buffer, [9; 64]);
        let n = gather(&mut [], TIMEOUT, TIMEOUT, |_, _| panic!("empty buffer")).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn gather_clips_an_overlong_piece_to_the_space_left() {
        let mut buffer = [0; 100];
        let n = gather(&mut buffer, TIMEOUT, TIMEOUT, |chunk, _| {
            chunk.fill(7);
            Ok(64)
        })
        .unwrap();
        assert_eq!(n, 100);
        assert_eq!(buffer, [7; 100]);
    }

    #[test]
    fn errors_describe_the_pipe() {
        assert_eq!(Error::NoPipe(0x81).to_string(), "the device has no pipe 0x81");
        assert_eq!(
            Error::PipeType(0x01, 1).to_string(),
            "pipe 0x01 has unusable transfer type 1"
        );
    }

    #[test]
    #[ignore = "opens the dongles"]
    fn dongles_open_and_close() {
        let dongles = crate::enumerate::find().unwrap();
        for (role, path) in [
            ("transmitter", &dongles.transmitter),
            ("receiver", &dongles.receiver),
        ] {
            let device = Device::open(path).unwrap();
            println!(
                "{role}: out {:?} in {:?}",
                device.out_pipe(),
                device.in_pipe()
            );
            assert_eq!(device.out_pipe().id, OUT_PIPE);
            assert_eq!(device.in_pipe().id, IN_PIPE);
            drop(device);
        }
    }
}
