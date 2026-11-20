#[cfg(unix)]
pub fn benchmark_resources() -> (u64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the value on success and retains no pointer.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return (0, 0);
        }
        usage.assume_init()
    };
    let micros = |time: libc::timeval| time.tv_sec as u64 * 1_000_000 + time.tv_usec as u64;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss as u64;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss as u64 * 1024;
    (micros(usage.ru_utime) + micros(usage.ru_stime), rss)
}

#[cfg(not(unix))]
pub fn benchmark_resources() -> (u64, u64) {
    (0, 0)
}
