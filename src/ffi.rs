//! The C interface: what a host written in another language calls.
//!
//! Every function returns 0 on success or a negative code, never panics
//! across the boundary, and leaves a description of the last failure for
//! [`lianli_last_error`]. Handles come from [`lianli_open`] and go back
//! through [`lianli_close`], which sends full speed to every reachable
//! group before it returns. The layouts here are fixed; see
//! `include/lianli_wireless.h`.

use crate::engine::{self, Engine, Snapshot};
use crate::process;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_char;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

/// Version of this interface's layouts and codes.
pub const ABI_VERSION: u32 = 1;

/// Most groups a state can carry.
pub const MAX_GROUPS: usize = 16;

/// Most log lines kept for the host between reads.
pub const LOG_CAPACITY: usize = 1000;

/// Success.
pub const OK: i32 = 0;
/// A pointer was null or a value out of range.
pub const ERR_ARGUMENT: i32 = -1;
/// The dongle could not be found, opened or connected.
pub const ERR_DONGLE: i32 = -2;
/// The L-Connect service is running and holds the dongle.
pub const ERR_LCONNECT: i32 = -3;
/// A panic was caught inside the call.
pub const ERR_PANIC: i32 = -4;
/// The caller's buffer or structure is too small.
pub const ERR_SIZE: i32 = -5;
/// The Windows call that lists processes failed.
pub const ERR_WINDOWS: i32 = -6;

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(text: impl Into<String>) {
    LAST_ERROR.with(|e| *e.borrow_mut() = text.into());
}

/// One group in a [`State`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Group {
    /// Address of the group's receiver.
    pub mac: [u8; 6],
    /// Receiver type.
    pub receiver: u8,
    /// Fans attached.
    pub fan_count: u8,
    /// Model byte of the first fan.
    pub model: u8,
    /// 1 when the group has been heard recently.
    pub online: u8,
    /// 1 when the receiver reports the target applied.
    pub acknowledged: u8,
    /// 1 when a target has been set.
    pub has_target: u8,
    /// Sends of the target since it was last seen applied.
    pub unacknowledged: u32,
    /// Speed of each fan.
    pub rpm: [u16; 4],
    /// Duty the receiver reports for each fan.
    pub duty: [u8; 4],
    /// Duties last sent.
    pub target: [u8; 4],
}

/// What the engine knows, laid out for a foreign caller.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// Bytes in this structure, set by the caller before reading.
    pub size: u32,
    /// [`ABI_VERSION`] of the library that filled it.
    pub version: u32,
    /// The dongle's address.
    pub master_mac: [u8; 6],
    /// The dongle's channel.
    pub channel: u8,
    /// 1 while the failsafe is in force.
    pub alarm: u8,
    /// Ticks run so far.
    pub ticks: u64,
    /// Polls that succeeded.
    pub polls: u64,
    /// Polls that failed.
    pub poll_failures: u64,
    /// Groups filled in below.
    pub group_count: u32,
    /// Fan groups bound to the dongle, in slot order.
    pub groups: [Group; MAX_GROUPS],
}

impl State {
    fn from(snapshot: &Snapshot) -> Self {
        let mut groups = [Group::default(); MAX_GROUPS];
        let count = snapshot.groups.len().min(MAX_GROUPS);
        for (out, g) in groups.iter_mut().zip(&snapshot.groups) {
            *out = Group {
                mac: g.mac,
                receiver: g.receiver,
                fan_count: g.fan_count,
                model: g.model,
                online: u8::from(g.online),
                acknowledged: u8::from(g.acknowledged),
                has_target: u8::from(g.target.is_some()),
                unacknowledged: g.unacknowledged,
                rpm: g.rpm,
                duty: g.duty,
                target: g.target.unwrap_or_default(),
            };
        }
        Self {
            size: std::mem::size_of::<State>() as u32,
            version: ABI_VERSION,
            master_mac: snapshot.master_mac,
            channel: snapshot.channel,
            alarm: u8::from(snapshot.alarm.is_some()),
            ticks: snapshot.ticks,
            polls: snapshot.polls,
            poll_failures: snapshot.poll_failures,
            group_count: count as u32,
            groups,
        }
    }
}

/// A running engine with the log lines it has produced.
pub struct Handle {
    engine: Option<Engine>,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl Handle {
    /// Wraps an engine started on any link, keeping its log.
    pub fn from_engine(engine: Engine, log: Arc<Mutex<VecDeque<String>>>) -> Self {
        Self {
            engine: Some(engine),
            log,
        }
    }

    /// A log sink for [`Engine::start`] that feeds a handle's log.
    pub fn log_sink() -> (Arc<Mutex<VecDeque<String>>>, impl FnMut(&str) + Send + 'static) {
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::clone(&log);
        let push = move |line: &str| {
            let mut lines = sink.lock().unwrap_or_else(|p| p.into_inner());
            if lines.len() >= LOG_CAPACITY {
                lines.pop_front();
            }
            lines.push_back(line.to_string());
        };
        (log, push)
    }
}

fn guarded(what: &str, call: impl FnOnce() -> i32) -> i32 {
    match panic::catch_unwind(AssertUnwindSafe(call)) {
        Ok(code) => code,
        Err(panic) => {
            set_error(format!("{what}: {}", engine::panic_text(&panic)));
            ERR_PANIC
        }
    }
}

/// The version of this interface.
#[no_mangle]
pub extern "C" fn lianli_version() -> u32 {
    ABI_VERSION
}

/// Finds the dongle, connects, and starts the engine. On success writes
/// a handle to `out`.
///
/// # Safety
/// `out` must be null or point to writable storage for a pointer.
#[no_mangle]
pub unsafe extern "C" fn lianli_open(out: *mut *mut Handle) -> i32 {
    guarded("open", || {
        if out.is_null() {
            set_error("open: out is null");
            return ERR_ARGUMENT;
        }
        match process::running(process::LCONNECT_SERVICE) {
            Ok(true) => {
                set_error(format!(
                    "{} is running and owns the dongle",
                    process::LCONNECT_SERVICE
                ));
                return ERR_LCONNECT;
            }
            Ok(false) => {}
            Err(error) => {
                set_error(error.to_string());
                return ERR_WINDOWS;
            }
        }
        let (log, sink) = Handle::log_sink();
        match Engine::open(sink) {
            Ok(engine) => {
                let handle = Box::new(Handle::from_engine(engine, log));
                unsafe { out.write(Box::into_raw(handle)) };
                OK
            }
            Err(error) => {
                set_error(error.to_string());
                ERR_DONGLE
            }
        }
    })
}

/// Stops the engine, after full speed to every reachable group, and
/// frees the handle.
///
/// # Safety
/// `handle` must be null or a pointer from [`lianli_open`] not yet closed.
#[no_mangle]
pub unsafe extern "C" fn lianli_close(handle: *mut Handle) -> i32 {
    guarded("close", || {
        if handle.is_null() {
            set_error("close: handle is null");
            return ERR_ARGUMENT;
        }
        let mut handle = unsafe { Box::from_raw(handle) };
        if let Some(engine) = handle.engine.take() {
            engine.stop();
        }
        OK
    })
}

/// Asks for a percentage on the group with this address. Applied on the
/// engine's next tick, never below its floor.
///
/// # Safety
/// `handle` must be from [`lianli_open`]; `mac` must point to six bytes.
#[no_mangle]
pub unsafe extern "C" fn lianli_set_percent(
    handle: *const Handle,
    mac: *const u8,
    percent: u8,
) -> i32 {
    guarded("set_percent", || {
        if handle.is_null() || mac.is_null() {
            set_error("set_percent: handle or mac is null");
            return ERR_ARGUMENT;
        }
        if percent > 100 {
            set_error(format!("set_percent: {percent} is over 100"));
            return ERR_ARGUMENT;
        }
        let handle = unsafe { &*handle };
        let mut address = [0u8; 6];
        address.copy_from_slice(unsafe { std::slice::from_raw_parts(mac, 6) });
        match &handle.engine {
            Some(engine) => {
                engine.set_percent(address, percent);
                OK
            }
            None => {
                set_error("set_percent: engine stopped");
                ERR_ARGUMENT
            }
        }
    })
}

/// Fills `state` with what the engine knew at its last tick. The caller
/// sets `state.size` first; a structure from an older layout is refused.
///
/// # Safety
/// `handle` must be from [`lianli_open`]; `state` must point to a
/// [`State`] whose `size` field is set.
#[no_mangle]
pub unsafe extern "C" fn lianli_read_state(handle: *const Handle, state: *mut State) -> i32 {
    guarded("read_state", || {
        if handle.is_null() || state.is_null() {
            set_error("read_state: handle or state is null");
            return ERR_ARGUMENT;
        }
        let expected = std::mem::size_of::<State>() as u32;
        let given = unsafe { (*state).size };
        if given < expected {
            set_error(format!(
                "read_state: state is {given} bytes, this library needs {expected}"
            ));
            return ERR_SIZE;
        }
        let handle = unsafe { &*handle };
        let Some(engine) = &handle.engine else {
            set_error("read_state: engine stopped");
            return ERR_ARGUMENT;
        };
        let filled = State::from(&engine.snapshot());
        unsafe { state.write(filled) };
        OK
    })
}

/// Copies the next log line into `buffer` as a NUL-terminated string.
/// Returns the line's length in bytes without the NUL, 0 when there is
/// no line waiting, or a negative code. A line longer than the buffer is
/// cut to fit.
///
/// # Safety
/// `handle` must be from [`lianli_open`]; `buffer` must have `length`
/// writable bytes.
#[no_mangle]
pub unsafe extern "C" fn lianli_take_log(
    handle: *const Handle,
    buffer: *mut c_char,
    length: usize,
) -> i32 {
    guarded("take_log", || {
        if handle.is_null() || buffer.is_null() || length == 0 {
            set_error("take_log: handle or buffer is null, or length is 0");
            return ERR_ARGUMENT;
        }
        let handle = unsafe { &*handle };
        let line = handle
            .log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front();
        match line {
            None => 0,
            Some(line) => unsafe { copy_out(&line, buffer, length) },
        }
    })
}

/// Copies the description of the last failure on this thread into
/// `buffer` as a NUL-terminated string. Returns its length in bytes
/// without the NUL, cut to fit.
///
/// # Safety
/// `buffer` must have `length` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn lianli_last_error(buffer: *mut c_char, length: usize) -> i32 {
    if buffer.is_null() || length == 0 {
        return ERR_ARGUMENT;
    }
    let text = LAST_ERROR.with(|e| e.borrow().clone());
    unsafe { copy_out(&text, buffer, length) }
}

unsafe fn copy_out(text: &str, buffer: *mut c_char, length: usize) -> i32 {
    let mut end = text.len().min(length - 1);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = &text.as_bytes()[..end];
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, end);
        buffer.add(end).write(0);
    }
    end as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fake::{device, reply, Fake, A};
    use std::ffi::CStr;
    use std::ptr;
    use std::thread;
    use std::time::Duration;

    fn last_error() -> String {
        let mut buffer = [0 as c_char; 256];
        let n = unsafe { lianli_last_error(buffer.as_mut_ptr(), buffer.len()) };
        assert!(n >= 0);
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .unwrap()
            .to_string()
    }

    fn open_fake() -> *mut Handle {
        let (log, sink) = Handle::log_sink();
        let engine = Engine::start(Fake::new(reply(&[device(A, 2, 3, 206)])), sink);
        Box::into_raw(Box::new(Handle::from_engine(engine, log)))
    }

    #[test]
    fn layouts_are_fixed() {
        assert_eq!(std::mem::size_of::<Group>(), 32);
        assert_eq!(std::mem::size_of::<State>(), 48 + 32 * MAX_GROUPS);
        assert_eq!(lianli_version(), 1);
    }

    #[test]
    fn null_arguments_are_refused_with_a_message() {
        assert_eq!(unsafe { lianli_open(ptr::null_mut()) }, ERR_ARGUMENT);
        assert_eq!(last_error(), "open: out is null");
        assert_eq!(unsafe { lianli_close(ptr::null_mut()) }, ERR_ARGUMENT);
        assert_eq!(
            unsafe { lianli_set_percent(ptr::null(), ptr::null(), 50) },
            ERR_ARGUMENT
        );
        assert_eq!(
            unsafe { lianli_read_state(ptr::null(), ptr::null_mut()) },
            ERR_ARGUMENT
        );
        let mut buffer = [0 as c_char; 8];
        assert_eq!(
            unsafe { lianli_take_log(ptr::null(), buffer.as_mut_ptr(), 8) },
            ERR_ARGUMENT
        );
        assert_eq!(unsafe { lianli_last_error(ptr::null_mut(), 8) }, ERR_ARGUMENT);
    }

    #[test]
    fn a_handle_reports_state_and_log_and_closes() {
        let handle = open_fake();
        let mac = A;
        assert_eq!(unsafe { lianli_set_percent(handle, mac.as_ptr(), 101) }, ERR_ARGUMENT);
        assert_eq!(unsafe { lianli_set_percent(handle, mac.as_ptr(), 50) }, OK);
        thread::sleep(Duration::from_millis(1300));

        let mut state = State {
            size: std::mem::size_of::<State>() as u32,
            version: 0,
            master_mac: [0; 6],
            channel: 0,
            alarm: 0,
            ticks: 0,
            polls: 0,
            poll_failures: 0,
            group_count: 0,
            groups: [Group::default(); MAX_GROUPS],
        };
        assert_eq!(unsafe { lianli_read_state(handle, &mut state) }, OK);
        assert_eq!(state.version, 1);
        assert_eq!(state.master_mac, [9; 6]);
        assert_eq!(state.channel, 8);
        assert!(state.ticks >= 2);
        assert_eq!(state.group_count, 1);
        let g = state.groups[0];
        assert_eq!(g.mac, A);
        assert_eq!(g.fan_count, 3);
        assert_eq!(g.online, 1);
        assert_eq!(g.has_target, 1);
        assert_eq!(g.target, [128, 128, 128, 0]);
        assert_eq!(g.rpm, [1800, 1800, 1800, 0]);

        let mut small = state;
        small.size = 8;
        assert_eq!(unsafe { lianli_read_state(handle, &mut small) }, ERR_SIZE);
        assert!(last_error().contains("8 bytes"));

        let mut buffer = [0 as c_char; 256];
        let n = unsafe { lianli_take_log(handle, buffer.as_mut_ptr(), buffer.len()) };
        assert!(n > 0);
        let first = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().unwrap();
        assert!(first.starts_with("engine started"), "{first}");
        let mut lines = 1;
        while unsafe { lianli_take_log(handle, buffer.as_mut_ptr(), buffer.len()) } > 0 {
            lines += 1;
        }
        assert!(lines >= 3, "{lines}");

        assert_eq!(unsafe { lianli_close(handle) }, OK);
    }

    #[test]
    fn long_text_is_cut_to_the_buffer_on_a_character_boundary() {
        let mut buffer = [0 as c_char; 5];
        let n = unsafe { copy_out("h\u{e9}llo world", buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(n, 4);
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().unwrap();
        assert_eq!(text, "h\u{e9}l");
        let mut buffer = [0 as c_char; 3];
        let n = unsafe { copy_out("h\u{e9}", buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(n, 1);
        let n = unsafe { copy_out("", buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(n, 0);
    }

    #[test]
    fn a_panic_inside_a_call_becomes_a_code() {
        let code = guarded("test", || panic!("boom"));
        assert_eq!(code, ERR_PANIC);
        assert_eq!(last_error(), "test: boom");
    }
}
