//! Process CPU time and peak memory, for benchmark and diagnostics reports.

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Usage {
    /// User plus kernel CPU time consumed by this process so far.
    pub cpu_ms: f64,
    /// Largest resident memory (working set) this process has used.
    pub peak_memory_mb: f64,
}

#[cfg(windows)]
pub fn usage() -> Option<Usage> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let zero = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    // FILETIME counts 100 ns intervals.
    let to_ms = |t: &FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 10_000.0
    };
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
    // every out-parameter is a valid local of the expected type and size.
    unsafe {
        let process = GetCurrentProcess();
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return None;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(process, &mut counters, counters.cb) == 0 {
            return None;
        }
        Some(Usage {
            cpu_ms: to_ms(&kernel) + to_ms(&user),
            peak_memory_mb: counters.PeakWorkingSetSize as f64 / 1_048_576.0,
        })
    }
}

#[cfg(unix)]
pub fn usage() -> Option<Usage> {
    let to_ms = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    // SAFETY: `usage` is a valid, writable rusage for the call to fill.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        usage
    };
    // The peak resident size is in bytes on macOS and in KiB elsewhere.
    let peak_bytes = if cfg!(target_os = "macos") {
        usage.ru_maxrss as f64
    } else {
        usage.ru_maxrss as f64 * 1024.0
    };
    Some(Usage {
        cpu_ms: to_ms(usage.ru_utime) + to_ms(usage.ru_stime),
        peak_memory_mb: peak_bytes / 1_048_576.0,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_usage() {
        let u = super::usage().expect("usage available");
        assert!(u.peak_memory_mb > 0.0);
        assert!(u.cpu_ms >= 0.0);
    }
}
