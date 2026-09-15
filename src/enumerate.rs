//! Finding the two dongles among the devices Windows lists.
//!
//! Both dongles register the same device interface. They are told apart
//! by the product id in their device paths.

use crate::win::*;
use std::ffi::c_void;
use std::fmt;
use std::mem;
use std::ptr;

/// The device interface both dongles register.
pub const INTERFACE_GUID: GUID = GUID {
    Data1: 0x1D4B_2365,
    Data2: 0x4749,
    Data3: 0x48EA,
    Data4: [0xB3, 0x8A, 0x7C, 0x6F, 0xDD, 0xDD, 0x7E, 0x26],
};

/// USB vendor id of the dongles.
pub const VENDOR: u16 = 0x0416;

/// USB product id of the transmitter.
pub const TRANSMITTER: u16 = 0x8040;

/// USB product id of the receiver.
pub const RECEIVER: u16 = 0x8041;

/// Which half of the dongle pair a device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Carries frames to the fans.
    Transmitter,
    /// Answers discovery requests.
    Receiver,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Transmitter => "transmitter",
            Self::Receiver => "receiver",
        })
    }
}

/// Device paths of the two dongles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dongles {
    /// Path to open the transmitter.
    pub transmitter: String,
    /// Path to open the receiver.
    pub receiver: String,
}

/// Why the dongles could not be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Windows refused to list devices.
    Windows(WinError),
    /// No device with this role is attached.
    Missing(Role),
    /// More than one device with this role is attached.
    Several(Role, usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => error.fmt(f),
            Self::Missing(role) => write!(f, "no {role} dongle is attached"),
            Self::Several(role, count) => write!(f, "{count} {role} dongles are attached"),
        }
    }
}

impl std::error::Error for Error {}

impl From<WinError> for Error {
    fn from(error: WinError) -> Self {
        Self::Windows(error)
    }
}

/// Finds the attached transmitter and receiver.
pub fn find() -> Result<Dongles, Error> {
    pair(&interfaces()?)
}

/// Picks the transmitter and receiver out of a list of device paths.
pub fn pair(paths: &[String]) -> Result<Dongles, Error> {
    let one = |role: Role| -> Result<String, Error> {
        let mut matching = paths.iter().filter(|p| classify(p) == Some(role));
        let first = matching.next().ok_or(Error::Missing(role))?;
        match matching.count() {
            0 => Ok(first.clone()),
            more => Err(Error::Several(role, more + 1)),
        }
    };
    Ok(Dongles {
        transmitter: one(Role::Transmitter)?,
        receiver: one(Role::Receiver)?,
    })
}

/// The role a device path names, from its vendor and product ids.
pub fn classify(path: &str) -> Option<Role> {
    let lower = path.to_ascii_lowercase();
    let vendor = format!("vid_{VENDOR:04x}&pid_");
    let rest = &lower[lower.find(&vendor)? + vendor.len()..];
    let product = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
    match product {
        TRANSMITTER => Some(Role::Transmitter),
        RECEIVER => Some(Role::Receiver),
        _ => None,
    }
}

/// Paths of every present device that registers the dongles' interface.
pub fn interfaces() -> Result<Vec<String>, WinError> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            &INTERFACE_GUID,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE {
        return Err(WinError::last("SetupDiGetClassDevsW"));
    }
    let result = walk(set);
    unsafe {
        SetupDiDestroyDeviceInfoList(set);
    }
    result
}

fn walk(set: HDEVINFO) -> Result<Vec<String>, WinError> {
    let mut paths = Vec::new();
    for index in 0.. {
        let mut data = SP_DEVICE_INTERFACE_DATA {
            cbSize: mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as DWORD,
            InterfaceClassGuid: INTERFACE_GUID,
            Flags: 0,
            Reserved: 0,
        };
        let found = unsafe {
            SetupDiEnumDeviceInterfaces(set, ptr::null(), &INTERFACE_GUID, index, &mut data)
        };
        if found == 0 {
            let error = WinError::last("SetupDiEnumDeviceInterfaces");
            if error.code == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(error);
        }

        let mut required: DWORD = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut data,
                ptr::null_mut(),
                0,
                &mut required,
                ptr::null_mut(),
            );
        }
        let error = WinError::last("SetupDiGetDeviceInterfaceDetailW");
        if error.code != ERROR_INSUFFICIENT_BUFFER {
            return Err(error);
        }

        let words = (required as usize).div_ceil(mem::size_of::<u32>()).max(2);
        let mut buffer = vec![0u32; words];
        buffer[0] = DEVICE_INTERFACE_DETAIL_CB_SIZE;
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut data,
                buffer.as_mut_ptr() as *mut c_void,
                required,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(WinError::last("SetupDiGetDeviceInterfaceDetailW"));
        }
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, required as usize) };
        let units: Vec<u16> = bytes[DEVICE_INTERFACE_DETAIL_PATH_OFFSET..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        paths.push(String::from_utf16_lossy(&units));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TX: &str =
        r"\\?\usb#vid_0416&pid_8040#b&25613874&0&3#{1d4b2365-4749-48ea-b38a-7c6fdddd7e26}";
    const RX: &str =
        r"\\?\usb#vid_0416&pid_8041#b&25613874&0&2#{1d4b2365-4749-48ea-b38a-7c6fdddd7e26}";

    #[test]
    fn interface_guid_reads_back_as_its_text() {
        let g = INTERFACE_GUID;
        let text = format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.Data1,
            g.Data2,
            g.Data3,
            g.Data4[0],
            g.Data4[1],
            g.Data4[2],
            g.Data4[3],
            g.Data4[4],
            g.Data4[5],
            g.Data4[6],
            g.Data4[7]
        );
        assert_eq!(text, "{1D4B2365-4749-48EA-B38A-7C6FDDDD7E26}");
    }

    #[test]
    fn paths_are_classified_by_product_id() {
        assert_eq!(classify(TX), Some(Role::Transmitter));
        assert_eq!(classify(RX), Some(Role::Receiver));
        assert_eq!(classify(&TX.to_uppercase()), Some(Role::Transmitter));
        assert_eq!(classify(r"\\?\usb#vid_0416&pid_8042#x#{g}"), None);
        assert_eq!(classify(r"\\?\usb#vid_1a86&pid_8040#x#{g}"), None);
        assert_eq!(classify(r"\\?\usb#vid_0416&pid_80#x"), None);
        assert_eq!(classify(""), None);
    }

    #[test]
    fn pair_takes_one_of_each() {
        let paths = vec![RX.to_string(), "other".to_string(), TX.to_string()];
        assert_eq!(
            pair(&paths),
            Ok(Dongles {
                transmitter: TX.to_string(),
                receiver: RX.to_string()
            })
        );
    }

    #[test]
    fn pair_reports_what_is_missing_or_doubled() {
        assert_eq!(
            pair(&[RX.to_string()]),
            Err(Error::Missing(Role::Transmitter))
        );
        assert_eq!(pair(&[TX.to_string()]), Err(Error::Missing(Role::Receiver)));
        assert_eq!(pair(&[]), Err(Error::Missing(Role::Transmitter)));
        assert_eq!(
            pair(&[TX.to_string(), RX.to_string(), RX.to_string()]),
            Err(Error::Several(Role::Receiver, 2))
        );
        assert_eq!(
            Error::Several(Role::Receiver, 2).to_string(),
            "2 receiver dongles are attached"
        );
        assert_eq!(
            Error::Missing(Role::Transmitter).to_string(),
            "no transmitter dongle is attached"
        );
    }

    #[test]
    #[ignore = "needs the dongles attached"]
    fn dongles_are_attached_here() {
        let paths = interfaces().unwrap();
        for path in &paths {
            println!("{path}");
        }
        let dongles = find().unwrap();
        assert_eq!(classify(&dongles.transmitter), Some(Role::Transmitter));
        assert_eq!(classify(&dongles.receiver), Some(Role::Receiver));
    }
}
