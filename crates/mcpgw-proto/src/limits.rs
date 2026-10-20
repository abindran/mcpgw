//! Memory caps for stdio servers (M3-05).
//!
//! - **Linux:** `RLIMIT_DATA` is set in the child before `exec`, so the
//!   kernel refuses any one process's allocations past the cap. (Not
//!   `RLIMIT_AS`: V8 and other JIT runtimes reserve far more address space
//!   than they use and would not start.) A watchdog also caps the whole
//!   process group's resident memory.
//! - **macOS:** the kernel enforces no memory rlimit, so the watchdog does:
//!   it sums the process group's resident memory every 250 ms and kills the
//!   group once it's over the cap.
//! - **Windows:** the server runs in a second, nested Job Object with
//!   `JOB_OBJECT_LIMIT_JOB_MEMORY`. The kernel caps the committed memory of
//!   the whole tree, and the job's peak usage says afterwards whether the
//!   cap was hit.

/// The cap, as `256MiB`.
pub(crate) fn human(bytes: u64) -> String {
    mcpgw_core::ByteSize(bytes).to_string()
}

/// Set `RLIMIT_DATA` in the child (called between fork and exec: only
/// async-signal-safe calls).
#[cfg(target_os = "linux")]
pub(crate) fn set_data_limit(bytes: u64) -> std::io::Result<()> {
    let lim = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    // SAFETY: setrlimit with a valid pointer is async-signal-safe.
    if unsafe { libc::setrlimit(libc::RLIMIT_DATA, &lim) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Resident memory of every process in process group `pgid`.
#[cfg(target_os = "linux")]
pub(crate) fn group_rss(pgid: u32) -> Option<u64> {
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    let mut total = 0;
    let mut found = false;
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (comm) state ppid pgrp …"; comm may hold spaces and parens
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let pgrp: Option<u32> = rest.split_whitespace().nth(2).and_then(|p| p.parse().ok());
        if pgrp != Some(pgid) {
            continue;
        }
        let pages: u64 = std::fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|r| r.parse().ok()))
            .unwrap_or(0);
        total += pages * page;
        found = true;
    }
    found.then_some(total)
}

/// Resident memory of every process in process group `pgid`.
#[cfg(target_os = "macos")]
pub(crate) fn group_rss(pgid: u32) -> Option<u64> {
    const PROC_PGRP_ONLY: u32 = 2;
    let mut pids = vec![0 as libc::pid_t; 256];
    // SAFETY: the buffer is valid for its byte length.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid,
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return None;
    }
    pids.truncate(bytes as usize / std::mem::size_of::<libc::pid_t>());
    let mut total = 0;
    for pid in pids.into_iter().filter(|p| *p > 0) {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `info` is valid for `size` bytes.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            )
        };
        if n == size {
            total += info.pti_resident_size;
        }
    }
    Some(total)
}

/// Other Unixes: no measurement.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn group_rss(_pgid: u32) -> Option<u64> {
    None
}

/// Kill process group `pgid`.
#[cfg(unix)]
pub(crate) fn kill_group(pgid: u32) {
    // SAFETY: killpg has no memory-safety preconditions.
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
}

/// Watch process group `pgid`; past `limit` bytes, record why in `note`
/// and kill the group.
#[cfg(unix)]
pub(crate) async fn watch(
    pgid: u32,
    limit: u64,
    note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let Some(rss) = group_rss(pgid) else {
            return; // the group is gone
        };
        if rss > limit {
            if let Ok(mut n) = note.lock() {
                *n = Some(format!(
                    "out of memory: killed at its {} limit (using {})",
                    human(limit),
                    human(rss / (1024 * 1024) * 1024 * 1024)
                ));
            }
            tracing::warn!(
                pgid,
                rss,
                limit,
                "stdio server over its memory limit; killing it"
            );
            kill_group(pgid);
            return;
        }
    }
}

/// A Job Object holding a server's process tree with a memory cap.
#[cfg(windows)]
pub(crate) struct LimitJob {
    job: windows_sys::Win32::Foundation::HANDLE,
    limit: u64,
}

// SAFETY: a job handle may be used from any thread.
#[cfg(windows)]
unsafe impl Send for LimitJob {}
#[cfg(windows)]
unsafe impl Sync for LimitJob {}

#[cfg(windows)]
impl LimitJob {
    /// Put process `pid` (and the children it starts) in a new job capped
    /// at `limit` bytes of committed memory.
    pub(crate) fn new(pid: u32, limit: u64) -> std::io::Result<LimitJob> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::{
                JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
                    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                    SetInformationJobObject,
                },
                Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
            },
        };
        let err = std::io::Error::last_os_error;
        // SAFETY: plain Win32 calls with valid arguments; handles are closed.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(err());
            }
            let me = LimitJob { job, limit };
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = limit as usize;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(err());
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(err());
            }
            let ok = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if ok == 0 {
                return Err(err());
            }
            Ok(me)
        }
    }

    /// Why the tree died, if it hit the cap: its peak use reached it.
    pub(crate) fn note(&self) -> Option<String> {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            QueryInformationJobObject,
        };
        // SAFETY: `info` is valid for its size.
        let peak = unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            let ok = QueryInformationJobObject(
                self.job,
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            );
            if ok == 0 {
                return None;
            }
            info.PeakJobMemoryUsed as u64
        };
        // allocations are refused at the cap, so the peak sits just below it
        (peak + 2 * 1024 * 1024 >= self.limit)
            .then(|| format!("out of memory: stopped at its {} limit", human(self.limit)))
    }
}

#[cfg(windows)]
impl Drop for LimitJob {
    fn drop(&mut self) {
        // SAFETY: we own the handle. Closing it doesn't kill the processes
        // (no KILL_ON_JOB_CLOSE); the outer job does that.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

/// Lines in a server's stderr that mean it ran out of memory.
pub fn mentions_oom(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    [
        "out of memory",
        "memory allocation of",
        "memoryerror",
        "allocation failed",
        "cannot allocate memory",
        "enomem",
    ]
    .iter()
    .any(|p| l.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oom_lines() {
        assert!(mentions_oom(
            "FATAL ERROR: Committing semi space failed. Allocation failed - JavaScript heap out of memory"
        ));
        assert!(mentions_oom("memory allocation of 1048576 bytes failed"));
        assert!(mentions_oom("MemoryError"));
        assert!(!mentions_oom("server ready"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn measures_own_group() {
        // our own process group holds at least this test process
        let pgid = unsafe { libc::getpgrp() } as u32;
        let rss = group_rss(pgid).expect("own group");
        assert!(rss > 1024 * 1024, "{rss}");
    }
}
