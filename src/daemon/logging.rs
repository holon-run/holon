//! Standalone-owned, size-bounded stdio logging. Foreground serve is untouched.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DAEMON_LOG_PATH_ENV: &str = "HOLON_INTERNAL_DAEMON_LOG_PATH";
pub(crate) const SEGMENT_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const SEGMENT_COUNT: usize = 4;
const BUFFER_BYTES: usize = 64 * 1024;
const STATUS_BYTES: usize = 4096;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonLogHealth {
    pub degraded: bool,
    pub buffered_bytes: usize,
    pub dropped_bytes: u64,
    pub discarded_history_bytes: u64,
    pub last_error: Option<String>,
}

fn segment_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    name.into()
}

fn status_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".status.json");
    name.into()
}

pub(crate) fn read_log_health(path: &Path) -> Result<Option<DaemonLogHealth>> {
    let path = status_path(path);
    // The single writer overwrites a fixed, preallocated diagnostic block.
    // Retry a concurrent overwrite, never scan an arbitrarily large sidecar.
    for attempt in 0..3 {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("failed to open daemon log health"),
        };
        let mut bytes = Vec::new();
        file.take(STATUS_BYTES as u64).read_to_end(&mut bytes)?;
        match serde_json::from_slice(&bytes) {
            Ok(health) => return Ok(Some(health)),
            Err(error) if attempt == 2 => {
                return Err(error).context("daemon log health is unavailable")
            }
            Err(_) => std::thread::yield_now(),
        }
    }
    unreachable!()
}

struct RollingLog {
    path: PathBuf,
    _ownership: File,
    file: Option<File>,
    length: u64,
    limit: u64,
    segments: usize,
}

impl RollingLog {
    fn open(path: &Path, limit: u64, segments: usize) -> io::Result<(Self, u64)> {
        assert!(limit > 0 && segments > 0);
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let ownership = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(PathBuf::from(lock_path))?;
        fs2::FileExt::try_lock_exclusive(&ownership)?;
        let mut discarded = 0;
        for index in 0..segments {
            let path = segment_path(path, index);
            let mut source = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let length = source.metadata()?.len();
            if !source.metadata()?.is_file() {
                return Err(io::Error::other("daemon log segment is not a regular file"));
            }
            if length <= limit {
                continue;
            }
            // Keep only a bounded suffix, including on the first upgrade from
            // an old multi-GB log. Never retain an unbudgeted giant backup.
            let mut temporary = path.as_os_str().to_os_string();
            temporary.push(".retained");
            let temporary = PathBuf::from(temporary);
            match fs::remove_file(&temporary) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let result = (|| -> io::Result<()> {
                let mut target = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                target.set_permissions(source.metadata()?.permissions())?;
                source.seek(SeekFrom::Start(length - limit))?;
                io::copy(&mut source.take(limit), &mut target)?;
                target.flush()?;
                fs::rename(&temporary, &path)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
            discarded += length - limit;
        }
        let mut log = Self {
            path: path.to_path_buf(),
            _ownership: ownership,
            file: None,
            length: 0,
            limit,
            segments,
        };
        log.reopen()?;
        Ok((log, discarded))
    }

    fn reopen(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let length = file.metadata()?.len();
        if length > self.limit {
            return Err(io::Error::other("daemon log exceeds segment limit"));
        }
        self.length = length;
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.take();
        let oldest = segment_path(&self.path, self.segments - 1);
        match fs::remove_file(oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for index in (1..self.segments).rev() {
            match fs::rename(
                segment_path(&self.path, index - 1),
                segment_path(&self.path, index),
            ) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.reopen()
    }
}

impl Write for RollingLog {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        if self.length == self.limit {
            self.rotate()?;
        }
        let count = bytes.len().min((self.limit - self.length) as usize);
        let written = self.file.as_mut().unwrap().write(&bytes[..count])?;
        self.length += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

struct LogCollector<W> {
    writer: W,
    status: File,
    health: DaemonLogHealth,
    pending: Vec<u8>,
    retry_at: Option<Instant>,
    last_status: Instant,
}

impl<W: Write> LogCollector<W> {
    fn new(writer: W, path: &Path, discarded_history_bytes: u64) -> io::Result<Self> {
        let status = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(status_path(path))?;
        // Reserve a fixed block before the daemon starts, so ordinary ENOSPC
        // can still update degradation counters without allocating more space.
        status.set_len(STATUS_BYTES as u64)?;
        let mut collector = Self {
            writer,
            status,
            health: DaemonLogHealth {
                discarded_history_bytes,
                ..Default::default()
            },
            pending: Vec::new(),
            retry_at: None,
            last_status: Instant::now(),
        };
        collector.publish()?;
        Ok(collector)
    }

    fn publish(&mut self) -> io::Result<()> {
        self.health.buffered_bytes = self.pending.len();
        let bytes = serde_json::to_vec(&self.health)?;
        if bytes.len() > STATUS_BYTES {
            return Err(io::Error::other("daemon log status exceeds fixed budget"));
        }
        let mut block = [b' '; STATUS_BYTES];
        block[..bytes.len()].copy_from_slice(&bytes);
        self.status.seek(SeekFrom::Start(0))?;
        self.status.write_all(&block)?;
        self.status.flush()?;
        self.last_status = Instant::now();
        Ok(())
    }

    fn retain(&mut self, bytes: &[u8]) {
        let excess = (self.pending.len() + bytes.len()).saturating_sub(BUFFER_BYTES);
        self.health.dropped_bytes = self.health.dropped_bytes.saturating_add(excess as u64);
        let remove = excess.min(self.pending.len());
        self.pending.drain(..remove);
        self.pending
            .extend_from_slice(&bytes[excess.saturating_sub(remove)..]);
    }

    fn failure(&mut self, error: io::Error) {
        self.health.degraded = true;
        self.health.last_error = Some(error.to_string().chars().take(512).collect());
        self.retry_at = Some(Instant::now() + RETRY_INTERVAL);
    }

    fn ingest(&mut self, bytes: &[u8]) {
        if self.retry_at.is_some_and(|retry| Instant::now() < retry) {
            self.retain(bytes);
            if self.last_status.elapsed() >= RETRY_INTERVAL {
                let _ = self.publish();
            }
            return;
        }
        let was_degraded = self.health.degraded;
        let pending = std::mem::take(&mut self.pending);
        let mut parts = [&pending[..], bytes];
        for index in 0..parts.len() {
            while !parts[index].is_empty() {
                match self.writer.write(parts[index]) {
                    Ok(0) => {
                        self.failure(io::Error::new(io::ErrorKind::WriteZero, "daemon log write"));
                        break;
                    }
                    Ok(count) => parts[index] = &parts[index][count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        self.failure(error);
                        break;
                    }
                }
            }
            if !parts[index].is_empty() {
                for rest in &parts[index..] {
                    self.retain(rest);
                }
                let _ = self.publish();
                return;
            }
        }
        self.health.degraded = false;
        self.retry_at = None;
        if was_degraded {
            let _ = self.publish();
        }
    }

    fn finish(&mut self) {
        // One final bounded retry, then account for bytes that cannot survive
        // process exit. Do not block daemon shutdown on a failed log target.
        self.retry_at = None;
        self.ingest(&[]);
        if let Err(error) = self.writer.flush() {
            self.failure(error);
        }
        self.health.dropped_bytes = self
            .health
            .dropped_bytes
            .saturating_add(self.pending.len() as u64);
        self.pending.clear();
        let _ = self.publish();
    }

    fn drain(&mut self, reader: &mut impl Read) {
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => self.ingest(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    // Retry and publish bounded loss even if a burst is
                    // followed by silence, not only when new output arrives.
                    self.ingest(&[]);
                }
                Err(error) => {
                    self.failure(error);
                    break;
                }
            }
        }
        self.finish();
    }
}

#[cfg(unix)]
mod stdio {
    use super::*;
    use std::{
        net::Shutdown,
        os::{
            fd::{AsFd, AsRawFd},
            unix::net::UnixStream,
        },
        thread::{self, JoinHandle},
    };

    pub struct ManagedDaemonLog {
        writer: UnixStream,
        drain: Option<JoinHandle<()>>,
        null: File,
    }

    fn redirect(source: &impl AsRawFd, target: i32) -> io::Result<()> {
        // SAFETY: source is live and target is a standard process descriptor.
        if unsafe { libc::dup2(source.as_raw_fd(), target) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl ManagedDaemonLog {
        pub fn from_env() -> Result<Option<Self>> {
            let Some(path) = std::env::var_os(DAEMON_LOG_PATH_ENV) else {
                return Ok(None);
            };
            // Run before tracing/Tokio start. Descendant holon commands must
            // not install a second collector on the same files.
            std::env::remove_var(DAEMON_LOG_PATH_ENV);
            Self::install(Path::new(&path), SEGMENT_BYTES, SEGMENT_COUNT).map(Some)
        }

        pub(super) fn install(path: &Path, limit: u64, segments: usize) -> Result<Self> {
            let (log, discarded) = RollingLog::open(path, limit, segments)
                .with_context(|| format!("failed to initialize bounded log {}", path.display()))?;
            let mut collector = LogCollector::new(log, path, discarded)?;
            if discarded > 0 {
                collector.ingest(
                    format!("daemon log retention: discarded {discarded} historical bytes\n")
                        .as_bytes(),
                );
            }
            let (mut reader, writer) = UnixStream::pair()?;
            reader.set_read_timeout(Some(RETRY_INTERVAL))?;
            let null = OpenOptions::new().write(true).open("/dev/null")?;
            io::stdout().flush()?;
            io::stderr().flush()?;
            let saved_stdout = io::stdout().as_fd().try_clone_to_owned()?;
            let drain = thread::Builder::new()
                .name("daemon-log-collector".into())
                .spawn(move || collector.drain(&mut reader))?;
            if let Err(error) = redirect(&writer, libc::STDOUT_FILENO)
                .and_then(|()| redirect(&writer, libc::STDERR_FILENO))
            {
                let _ = redirect(&saved_stdout, libc::STDOUT_FILENO);
                let _ = writer.shutdown(Shutdown::Write);
                let _ = drain.join();
                return Err(error).context("failed to capture daemon stdout/stderr");
            }
            Ok(Self {
                writer,
                drain: Some(drain),
                null,
            })
        }
    }

    impl Drop for ManagedDaemonLog {
        fn drop(&mut self) {
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            let _ = redirect(&self.null, libc::STDOUT_FILENO);
            let _ = redirect(&self.null, libc::STDERR_FILENO);
            // Shutdown also fences inherited write descriptors in descendants,
            // so they cannot keep the collector alive after daemon shutdown.
            let _ = self.writer.shutdown(Shutdown::Write);
            if let Some(drain) = self.drain.take() {
                let _ = drain.join();
            }
        }
    }
}

#[cfg(unix)]
pub use stdio::ManagedDaemonLog;

#[cfg(not(unix))]
pub struct ManagedDaemonLog;

#[cfg(not(unix))]
impl ManagedDaemonLog {
    pub fn from_env() -> Result<Option<Self>> {
        anyhow::ensure!(
            std::env::var_os(DAEMON_LOG_PATH_ENV).is_none(),
            "managed daemon stdio logging is supported only on Unix"
        );
        Ok(None)
    }
}
