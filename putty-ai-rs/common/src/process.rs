//! Broker lifetime is independent of stdio MCP and of the launcher shell.
use crate::security::wide;
use anyhow::{Result, ensure};
use std::path::Path;
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};

pub fn start_broker(path: &Path) -> Result<()> {
    let application = wide(&path.to_string_lossy());
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        // Never inherit the launcher's pipes or unrelated inheritable handles.
        let ok = CreateProcessW(
            application.as_ptr(),
            std::ptr::null_mut(),
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
