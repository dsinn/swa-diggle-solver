//! Process enumeration, so we can refuse to launch a second game instance.
//!
//! Two concurrent instances share one save directory. `mainSaveData` has a single writer,
//! `overworld:save()` (`overworld.lua:562-568`), called on screen exit — so the loser is not
//! merged, it is overwritten, and a long-lived instance holding authoritative in-memory state
//! will silently clobber the other on its next screen exit. Nothing warns you; you just find
//! progress missing later. Cheaper to refuse than to detect.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// PIDs of every running process whose image name matches `exe_name`, case-insensitively.
pub fn pids_by_name(exe_name: &str) -> Result<Vec<u32>, crate::Error> {
    let want = exe_name.to_ascii_lowercase();
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_ascii_lowercase();
                if name == want {
                    out.push(e.th32ProcessID);
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    Ok(out)
}

/// Errors if any instance of `exe_name` is already running, excluding `allow`.
///
/// Call this before every launch. "I thought I closed it" is not a safety property.
pub fn refuse_if_running(exe_name: &str, allow: &[u32]) -> Result<(), crate::Error> {
    let live: Vec<u32> =
        pids_by_name(exe_name)?.into_iter().filter(|p| !allow.contains(p)).collect();
    if live.is_empty() {
        return Ok(());
    }
    Err(crate::Error::Win32(format!(
        "{} instance(s) of {exe_name} already running (pids {live:?}). Two instances share one \
         save directory and the second one to exit a screen overwrites the first. Close them \
         before launching.",
        live.len()
    )))
}
