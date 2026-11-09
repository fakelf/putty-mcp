//! Broker lifetime is independent of stdio MCP and of the launcher shell.
use anyhow::{Result, ensure};
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};

pub fn start_broker(path: &Path) -> Result<()> {
    let exe: Vec<u16> = path.as_os_str().encode_wide().collect();
    let application: Vec<u16> = exe.iter().copied().chain(Some(0)).collect();
    // CreateProcessW takes a mutable command line, including argv[0].
    let mut command_line: Vec<u16> = std::iter::once(b'"' as u16)
        .chain(exe)
        .chain("\" --broker".encode_utf16())
        .chain(Some(0))
        .collect();
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        // Never inherit the launcher's pipes or unrelated inheritable handles.
        let ok = CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        );
        ensure!(
            ok != 0,
            "cannot start independent broker: {}",
            std::io::Error::last_os_error()
        );
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}
