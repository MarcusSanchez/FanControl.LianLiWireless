//! Which programs are running, for the check that keeps two owners off
//! the dongle.

use crate::win::*;
use std::mem;

/// The L-Connect service, which holds the dongle whenever it runs.
pub const LCONNECT_SERVICE: &str = "L-Connect-Service.exe";

/// Whether a process with this executable name is running. The
/// comparison ignores ASCII case.
pub fn running(name: &str) -> Result<bool, WinError> {
    Ok(names()?.iter().any(|n| n.eq_ignore_ascii_case(name)))
}

/// Executable names of every running process.
pub fn names() -> Result<Vec<String>, WinError> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(WinError::last("CreateToolhelp32Snapshot"));
    }
    let result = walk(snapshot);
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

fn walk(snapshot: HANDLE) -> Result<Vec<String>, WinError> {
    let mut entry: PROCESSENTRY32W = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as DWORD;
    let mut names = Vec::new();
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    if more == 0 {
        return Err(WinError::last("Process32FirstW"));
    }
    while more != 0 {
        let end = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        names.push(String::from_utf16_lossy(&entry.szExeFile[..end]));
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    let error = WinError::last("Process32NextW");
    if error.code != ERROR_NO_MORE_FILES {
        return Err(error);
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_listed_and_a_made_up_one_is_not() {
        let me = std::env::current_exe().unwrap();
        let me = me.file_name().unwrap().to_str().unwrap().to_string();
        assert!(running(&me).unwrap());
        assert!(running(&me.to_ascii_uppercase()).unwrap());
        assert!(!running("no-such-program-4f2a9c.exe").unwrap());
    }

    #[test]
    fn the_listing_has_the_system_in_it() {
        let names = names().unwrap();
        assert!(names.len() > 10);
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case("explorer.exe")));
    }
}
