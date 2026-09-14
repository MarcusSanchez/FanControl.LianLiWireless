//! Hand-declared Win32 bindings for the calls the transport needs: device
//! interface enumeration, opening the device, and WinUSB pipe I/O.

#![allow(
    missing_docs,
    non_camel_case_types,
    non_snake_case,
    clippy::upper_case_acronyms
)]

use std::ffi::c_void;
use std::fmt;
use std::ptr;

pub type HANDLE = *mut c_void;
pub type HDEVINFO = *mut c_void;
pub type WINUSB_INTERFACE_HANDLE = *mut c_void;
pub type BOOL = i32;
pub type DWORD = u32;

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

pub const ERROR_FILE_NOT_FOUND: DWORD = 2;
pub const ERROR_ACCESS_DENIED: DWORD = 5;
pub const ERROR_INVALID_HANDLE: DWORD = 6;
pub const ERROR_GEN_FAILURE: DWORD = 31;
pub const ERROR_SHARING_VIOLATION: DWORD = 32;
pub const ERROR_SEM_TIMEOUT: DWORD = 121;
pub const ERROR_INSUFFICIENT_BUFFER: DWORD = 122;
pub const ERROR_NO_MORE_ITEMS: DWORD = 259;
pub const ERROR_OPERATION_ABORTED: DWORD = 995;
pub const ERROR_DEVICE_NOT_CONNECTED: DWORD = 1167;
pub const ERROR_DEVICE_REMOVED: DWORD = 1617;

pub const GENERIC_READ: DWORD = 0x8000_0000;
pub const GENERIC_WRITE: DWORD = 0x4000_0000;
pub const FILE_SHARE_READ: DWORD = 0x0001;
pub const FILE_SHARE_WRITE: DWORD = 0x0002;
pub const OPEN_EXISTING: DWORD = 3;
pub const FILE_ATTRIBUTE_NORMAL: DWORD = 0x0080;
pub const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;

pub const FORMAT_MESSAGE_IGNORE_INSERTS: DWORD = 0x0000_0200;
pub const FORMAT_MESSAGE_FROM_SYSTEM: DWORD = 0x0000_1000;

pub const DIGCF_PRESENT: DWORD = 0x0000_0002;
pub const DIGCF_DEVICEINTERFACE: DWORD = 0x0000_0010;

/// WinUSB pipe policies, for `WinUsb_SetPipePolicy`.
pub const SHORT_PACKET_TERMINATE: DWORD = 0x01;
pub const AUTO_CLEAR_STALL: DWORD = 0x02;
pub const PIPE_TRANSFER_TIMEOUT: DWORD = 0x03;
pub const IGNORE_SHORT_PACKETS: DWORD = 0x04;
pub const ALLOW_PARTIAL_READS: DWORD = 0x05;
pub const AUTO_FLUSH: DWORD = 0x06;
pub const RAW_IO: DWORD = 0x07;

/// Pipe types in `WINUSB_PIPE_INFORMATION::PipeType`.
pub const USBD_PIPE_TYPE_CONTROL: i32 = 0;
pub const USBD_PIPE_TYPE_ISOCHRONOUS: i32 = 1;
pub const USBD_PIPE_TYPE_BULK: i32 = 2;
pub const USBD_PIPE_TYPE_INTERRUPT: i32 = 3;

/// Bit set in a pipe id for an IN (device to host) endpoint.
pub const ENDPOINT_DIRECTION_IN: u8 = 0x80;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GUID {
    pub Data1: u32,
    pub Data2: u16,
    pub Data3: u16,
    pub Data4: [u8; 8],
}

#[repr(C)]
pub struct SP_DEVICE_INTERFACE_DATA {
    pub cbSize: DWORD,
    pub InterfaceClassGuid: GUID,
    pub Flags: DWORD,
    pub Reserved: usize,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default, Debug)]
pub struct USB_INTERFACE_DESCRIPTOR {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub bInterfaceNumber: u8,
    pub bAlternateSetting: u8,
    pub bNumEndpoints: u8,
    pub bInterfaceClass: u8,
    pub bInterfaceSubClass: u8,
    pub bInterfaceProtocol: u8,
    pub iInterface: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct WINUSB_PIPE_INFORMATION {
    pub PipeType: i32,
    pub PipeId: u8,
    pub MaximumPacketSize: u16,
    pub Interval: u8,
}

#[cfg(not(target_pointer_width = "64"))]
compile_error!("only 64-bit Windows is supported: the device interface detail size below is the 64-bit value");

/// `cbSize` of `SP_DEVICE_INTERFACE_DETAIL_DATA_W` on 64-bit Windows: the
/// `DWORD` plus one `WCHAR`, padded to the structure's alignment.
pub const DEVICE_INTERFACE_DETAIL_CB_SIZE: DWORD = 8;
/// Byte offset of `DevicePath` inside `SP_DEVICE_INTERFACE_DETAIL_DATA_W`.
pub const DEVICE_INTERFACE_DETAIL_PATH_OFFSET: usize = 4;

#[link(name = "setupapi")]
extern "system" {
    pub fn SetupDiGetClassDevsW(
        ClassGuid: *const GUID,
        Enumerator: *const u16,
        hwndParent: *mut c_void,
        Flags: DWORD,
    ) -> HDEVINFO;
    pub fn SetupDiEnumDeviceInterfaces(
        DeviceInfoSet: HDEVINFO,
        DeviceInfoData: *const c_void,
        InterfaceClassGuid: *const GUID,
        MemberIndex: DWORD,
        DeviceInterfaceData: *mut SP_DEVICE_INTERFACE_DATA,
    ) -> BOOL;
    pub fn SetupDiGetDeviceInterfaceDetailW(
        DeviceInfoSet: HDEVINFO,
        DeviceInterfaceData: *mut SP_DEVICE_INTERFACE_DATA,
        DeviceInterfaceDetailData: *mut c_void,
        DeviceInterfaceDetailDataSize: DWORD,
        RequiredSize: *mut DWORD,
        DeviceInfoData: *mut c_void,
    ) -> BOOL;
    pub fn SetupDiDestroyDeviceInfoList(DeviceInfoSet: HDEVINFO) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetLastError() -> DWORD;
    pub fn FormatMessageW(
        dwFlags: DWORD,
        lpSource: *const c_void,
        dwMessageId: DWORD,
        dwLanguageId: DWORD,
        lpBuffer: *mut u16,
        nSize: DWORD,
        Arguments: *mut c_void,
    ) -> DWORD;
    pub fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *mut c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn LoadLibraryW(lpLibFileName: *const u16) -> HANDLE;
    pub fn GetProcAddress(hModule: HANDLE, lpProcName: *const u8) -> *const c_void;
}

pub type WinUsb_Initialize =
    unsafe extern "system" fn(HANDLE, *mut WINUSB_INTERFACE_HANDLE) -> BOOL;
pub type WinUsb_Free = unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE) -> BOOL;
pub type WinUsb_QueryInterfaceSettings =
    unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE, u8, *mut USB_INTERFACE_DESCRIPTOR) -> BOOL;
pub type WinUsb_QueryPipe =
    unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE, u8, u8, *mut WINUSB_PIPE_INFORMATION) -> BOOL;
pub type WinUsb_SetPipePolicy =
    unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE, u8, DWORD, DWORD, *const c_void) -> BOOL;
pub type WinUsb_ReadPipe = unsafe extern "system" fn(
    WINUSB_INTERFACE_HANDLE,
    u8,
    *mut u8,
    DWORD,
    *mut DWORD,
    *mut c_void,
) -> BOOL;
pub type WinUsb_WritePipe = unsafe extern "system" fn(
    WINUSB_INTERFACE_HANDLE,
    u8,
    *const u8,
    DWORD,
    *mut DWORD,
    *mut c_void,
) -> BOOL;
pub type WinUsb_FlushPipe = unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE, u8) -> BOOL;
pub type WinUsb_AbortPipe = unsafe extern "system" fn(WINUSB_INTERFACE_HANDLE, u8) -> BOOL;

/// The entry points of `winusb.dll`, resolved when first needed.
///
/// The library is loaded by name at run time rather than linked, so the
/// build needs no import library for it.
#[derive(Clone, Copy)]
pub struct WinUsb {
    pub Initialize: WinUsb_Initialize,
    pub Free: WinUsb_Free,
    pub QueryInterfaceSettings: WinUsb_QueryInterfaceSettings,
    pub QueryPipe: WinUsb_QueryPipe,
    pub SetPipePolicy: WinUsb_SetPipePolicy,
    pub ReadPipe: WinUsb_ReadPipe,
    pub WritePipe: WinUsb_WritePipe,
    pub FlushPipe: WinUsb_FlushPipe,
    pub AbortPipe: WinUsb_AbortPipe,
}

impl WinUsb {
    /// The resolved entry points, loading the library on the first call.
    pub fn get() -> Result<&'static WinUsb, WinError> {
        static LOADED: std::sync::OnceLock<Result<WinUsb, WinError>> = std::sync::OnceLock::new();
        LOADED.get_or_init(Self::load).as_ref().map_err(|e| *e)
    }

    fn load() -> Result<WinUsb, WinError> {
        let module = unsafe { LoadLibraryW(wide("winusb.dll").as_ptr()) };
        if module.is_null() {
            return Err(WinError::last("LoadLibraryW"));
        }
        unsafe fn entry<F: Copy>(module: HANDLE, name: &'static [u8]) -> Result<F, WinError> {
            debug_assert_eq!(name.last(), Some(&0));
            let address = unsafe { GetProcAddress(module, name.as_ptr()) };
            if address.is_null() {
                return Err(WinError::last("GetProcAddress"));
            }
            debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const c_void>());
            Ok(unsafe { std::mem::transmute_copy::<*const c_void, F>(&address) })
        }
        unsafe {
            Ok(WinUsb {
                Initialize: entry(module, b"WinUsb_Initialize\0")?,
                Free: entry(module, b"WinUsb_Free\0")?,
                QueryInterfaceSettings: entry(module, b"WinUsb_QueryInterfaceSettings\0")?,
                QueryPipe: entry(module, b"WinUsb_QueryPipe\0")?,
                SetPipePolicy: entry(module, b"WinUsb_SetPipePolicy\0")?,
                ReadPipe: entry(module, b"WinUsb_ReadPipe\0")?,
                WritePipe: entry(module, b"WinUsb_WritePipe\0")?,
                FlushPipe: entry(module, b"WinUsb_FlushPipe\0")?,
                AbortPipe: entry(module, b"WinUsb_AbortPipe\0")?,
            })
        }
    }
}

/// A failed Win32 call: which one, and the error code it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinError {
    /// Name of the call that failed.
    pub call: &'static str,
    /// The error code from `GetLastError`.
    pub code: DWORD,
}

impl WinError {
    /// The error the last Win32 call on this thread left, attributed to
    /// `call`.
    pub fn last(call: &'static str) -> Self {
        let code = unsafe { GetLastError() };
        Self { call, code }
    }

    /// The system's description of the error code, or empty if it has none.
    pub fn message(&self) -> String {
        let mut buffer = [0u16; 512];
        let written = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                ptr::null(),
                self.code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as DWORD,
                ptr::null_mut(),
            )
        } as usize;
        String::from_utf16_lossy(&buffer[..written])
            .trim_end()
            .to_string()
    }
}

impl fmt::Display for WinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with error {}", self.call, self.code)?;
        let message = self.message();
        if !message.is_empty() {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for WinError {}

/// A string as Windows wants it: UTF-16 with a terminating zero.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn structures_have_their_windows_sizes() {
        assert_eq!(size_of::<GUID>(), 16);
        assert_eq!(size_of::<SP_DEVICE_INTERFACE_DATA>(), 32);
        assert_eq!(size_of::<USB_INTERFACE_DESCRIPTOR>(), 9);
        assert_eq!(size_of::<WINUSB_PIPE_INFORMATION>(), 12);
    }

    #[test]
    fn winusb_entry_points_resolve() {
        let winusb = WinUsb::get().unwrap();
        let again = WinUsb::get().unwrap();
        assert_eq!(winusb.Initialize as usize, again.Initialize as usize);
        let addresses = [
            winusb.Initialize as usize,
            winusb.Free as usize,
            winusb.QueryInterfaceSettings as usize,
            winusb.QueryPipe as usize,
            winusb.SetPipePolicy as usize,
            winusb.ReadPipe as usize,
            winusb.WritePipe as usize,
            winusb.FlushPipe as usize,
            winusb.AbortPipe as usize,
        ];
        assert!(addresses.iter().all(|a| *a != 0));
        let mut unique = addresses.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), addresses.len());
    }

    #[test]
    fn error_names_the_call_and_code() {
        let error = WinError {
            call: "CreateFileW",
            code: ERROR_FILE_NOT_FOUND,
        };
        let text = error.to_string();
        assert!(text.starts_with("CreateFileW failed with error 2"), "{text}");
        assert!(!error.message().is_empty());
    }

    #[test]
    fn unknown_code_has_no_message() {
        let error = WinError {
            call: "X",
            code: 0xFFFF_FFF0,
        };
        assert_eq!(error.message(), "");
        assert_eq!(error.to_string(), "X failed with error 4294967280");
    }

    #[test]
    fn wide_strings_end_in_zero() {
        assert_eq!(wide("ab"), vec![0x61, 0x62, 0]);
        assert_eq!(wide(""), vec![0]);
    }
}
