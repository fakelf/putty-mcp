//! Named pipes are local-only, DACL-protected and checked against the actual
//! peer process token. Same-user processes are the trust boundary, not malware isolation.
use anyhow::{Result, ensure};
use std::{ffi::c_void, os::windows::io::AsRawHandle};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    System::{Pipes::*, Threading::*},
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn sid_for_process(pid: u32) -> Result<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(!process.is_null(), "cannot open IPC peer process");
        let mut token = std::ptr::null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        ensure!(opened != 0, "cannot query IPC peer token");
        let mut size = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        // Allocate pointer-aligned storage for TOKEN_USER.
        let mut buf = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        ensure!(ok != 0, "cannot read IPC peer identity");
        let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let mut sid = std::ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "invalid SID"
        );
        let mut n = 0;
        while *sid.add(n) != 0 {
            n += 1;
        }
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(sid, n));
        LocalFree(sid.cast());
        Ok(result)
    }
}
pub fn current_sid() -> Result<String> {
    unsafe { sid_for_process(GetCurrentProcessId()) }
}
pub fn pipe_name() -> Result<String> {
    Ok(format!(r"\\.\pipe\putty-ai-v440-{}", current_sid()?))
}
pub fn peer_pid(pipe: &impl AsRawHandle, server: bool) -> Result<u32> {
    unsafe {
        let mut pid = 0;
        let ok = if server {
            GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid)
        } else {
            GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid)
        };
        ensure!(ok != 0, "cannot identify IPC peer");
        ensure!(
            sid_for_process(pid)? == current_sid()?,
            "IPC peer user mismatch"
        );
        Ok(pid)
    }
}
pub fn process_start(pid: u32) -> Result<u64> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(!p.is_null(), "peer exited");
        let mut c = std::mem::zeroed();
        let mut e = std::mem::zeroed();
        let mut k = std::mem::zeroed();
        let mut u = std::mem::zeroed();
        let ok = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(p);
        ensure!(ok != 0, "cannot verify process start");
        Ok((c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64)
    }
}

/// CPU time in 100 ns units and peak working set; used by isolated benchmarks.
pub fn process_usage(pid: u32) -> Result<(u64, usize)> {
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::*;
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        ensure!(!p.is_null(), "cannot open measured process");
        let mut c = std::mem::zeroed();
        let mut e = std::mem::zeroed();
        let mut k = std::mem::zeroed();
        let mut u = std::mem::zeroed();
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let ok = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u) != 0
            && K32GetProcessMemoryInfo(
                p,
                &mut memory,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ) != 0;
        CloseHandle(p);
        ensure!(ok, "cannot measure process");
        let cpu = (((k.dwHighDateTime as u64) << 32) | k.dwLowDateTime as u64)
            + (((u.dwHighDateTime as u64) << 32) | u.dwLowDateTime as u64);
        Ok((cpu, memory.PeakWorkingSetSize))
    }
}
pub struct PipeAcl {
    descriptor: *mut c_void,
    pub attributes: SECURITY_ATTRIBUTES,
}
impl PipeAcl {
    pub fn new() -> Result<Self> {
        unsafe {
            let sddl = wide(&format!("D:P(A;;GA;;;{})", current_sid()?));
            let mut descriptor = std::ptr::null_mut();
            ensure!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut()
                ) != 0,
                "cannot create pipe ACL"
            );
            Ok(Self {
                descriptor,
                attributes: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
            })
        }
    }
}
impl Drop for PipeAcl {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

pub fn restrict_path(path: &std::path::Path) -> Result<()> {
    let acl = PipeAcl::new()?;
    unsafe {
        ensure!(
            SetFileSecurityW(
                wide(&path.to_string_lossy()).as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                acl.descriptor
            ) != 0,
            "cannot restrict audit file ACL"
        );
    }
    Ok(())
}
