use std::{
    collections::HashSet,
    fmt,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use flate2::read::GzDecoder;
use reqwest::Url;
use serde_json::Value as JsonValue;
use tar::Archive;
use tempfile::{NamedTempFile, TempDir};

pub const DEFAULT_ARCHIVE_BASE: &str = "https://codeload.github.com";

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const SHA_RESOLVE_TIMEOUT: Duration = Duration::from_secs(15);
/// Total extracted size kept in the archive cache before oldest entries are
/// evicted. Entries younger than `ARCHIVE_CACHE_MIN_AGE` are never evicted so
/// an in-flight install cannot lose files under its reader.
const MAX_ARCHIVE_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const ARCHIVE_CACHE_MIN_AGE: Duration = Duration::from_secs(15 * 60);

#[derive(Debug)]
pub struct ArchiveHttpFailure {
    pub status: u16,
    pub retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub(crate) struct ArchiveDirectoryNotFound {
    pub path: PathBuf,
}

impl fmt::Display for ArchiveDirectoryNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "archive directory does not exist: {}",
            self.path.display()
        )
    }
}

impl std::error::Error for ArchiveDirectoryNotFound {}

impl fmt::Display for ArchiveHttpFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.retry_after {
            Some(retry_after) => write!(
                f,
                "GitHub archive request failed (HTTP {}; retry after {})",
                self.status,
                retry_after.to_rfc3339()
            ),
            None => write!(f, "GitHub archive request failed (HTTP {})", self.status),
        }
    }
}

impl std::error::Error for ArchiveHttpFailure {}

#[derive(Debug)]
pub struct GitHubArchive {
    #[allow(dead_code)]
    temp_dir: Option<TempDir>,
    root: PathBuf,
    rejected_entries: Vec<PathBuf>,
}

impl GitHubArchive {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn from_test_root(temp_dir: TempDir) -> Self {
        let root = temp_dir.path().to_path_buf();
        Self {
            temp_dir: Some(temp_dir),
            root,
            rejected_entries: Vec::new(),
        }
    }

    pub async fn fetch(
        client: &crate::client::AsyncClient,
        owner: &str,
        repo: &str,
        reference: &str,
        archive_base: &str,
    ) -> anyhow::Result<Self> {
        let archive_url = archive_url(archive_base, owner, repo, reference)?;
        let response = tokio::time::timeout(
            REQUEST_TIMEOUT,
            client
                .get(archive_url.clone())
                .timeout(REQUEST_TIMEOUT)
                .send(),
        )
        .await
        .context("GitHub archive request timed out")??;

        if response.status().is_success() {
            return tokio::time::timeout(REQUEST_TIMEOUT, unpack_async_response(response))
                .await
                .context("GitHub archive download timed out")?;
        }

        let failure = http_failure(response.status().as_u16(), response.headers());
        if client.auth_present() && matches!(failure.status, 401 | 403 | 404) {
            let rest_url = client.api_url(&rest_archive_url(owner, repo, reference)?)?;
            let response = tokio::time::timeout(
                REQUEST_TIMEOUT,
                client
                    .get(Url::parse(&rest_url)?)
                    .timeout(REQUEST_TIMEOUT)
                    .send(),
            )
            .await
            .context("GitHub REST archive fallback timed out")??;
            if !response.status().is_success() {
                return Err(async_http_error(client, response).await);
            }
            return tokio::time::timeout(REQUEST_TIMEOUT, unpack_async_response(response))
                .await
                .context("GitHub REST archive download timed out")?;
        }

        Err(anyhow::Error::new(failure))
    }

    pub fn fetch_blocking(
        client: &crate::client::BlockingClient,
        owner: &str,
        repo: &str,
        reference: &str,
        archive_base: &str,
    ) -> anyhow::Result<Self> {
        let archive_url = archive_url(archive_base, owner, repo, reference)?;
        let response = client
            .get(archive_url.as_str())
            .timeout(REQUEST_TIMEOUT)
            .send()?;
        if response.status().is_success() {
            let compressed = write_blocking_response(response)?;
            return Self::from_compressed_file(compressed.path());
        }

        let failure = http_failure(response.status().as_u16(), response.headers());
        if client.auth_present() && matches!(failure.status, 401 | 403 | 404) {
            let rest_url = client.api_url(&rest_archive_url(owner, repo, reference)?)?;
            let response = client.get(&rest_url).timeout(REQUEST_TIMEOUT).send()?;
            if !response.status().is_success() {
                return Err(blocking_http_error(client, response));
            }
            let compressed = write_blocking_response(response)?;
            return Self::from_compressed_file(compressed.path());
        }

        Err(anyhow::Error::new(failure))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn directory(&self, path: &str) -> anyhow::Result<PathBuf> {
        let relative = checked_directory_path(path)?;
        if self
            .rejected_entries
            .iter()
            .any(|entry| relative == *entry || entry.starts_with(&relative))
        {
            anyhow::bail!("selected archive directory contains unsupported entries");
        }
        let directory = self.root.join(&relative);
        if !directory.is_dir() {
            return Err(anyhow::Error::new(ArchiveDirectoryNotFound {
                path: relative,
            }));
        }
        Ok(directory)
    }

    fn from_compressed_file(path: &Path) -> anyhow::Result<Self> {
        validate_archive_limits(path)?;
        let file = File::open(path)?;
        let decoder = GzDecoder::new(BufReader::new(file));
        let mut archive = Archive::new(decoder);
        let temp_dir = tempfile::tempdir()?;
        let mut seen = HashSet::new();
        let mut rejected_entries = Vec::new();
        let mut root_name: Option<String> = None;
        let mut extracted_bytes = 0_u64;
        let mut entry_count = 0_usize;

        for entry in archive.entries()? {
            let mut entry = entry?;
            entry_count += 1;
            anyhow::ensure!(
                entry_count <= MAX_ENTRIES,
                "GitHub archive contains too many entries"
            );

            let entry_type = entry.header().entry_type();
            // Global PAX metadata is not content; raw limits already include it.
            if entry_type.is_pax_global_extensions() {
                continue;
            }

            let path = checked_archive_path(&entry.path()?)?;
            anyhow::ensure!(
                seen.insert(path.clone()),
                "GitHub archive contains a duplicate path: {}",
                path.display()
            );

            let mut components = path.components();
            let current_root = match components.next() {
                Some(Component::Normal(name)) => name.to_string_lossy().into_owned(),
                _ => anyhow::bail!("GitHub archive entry has no top-level root"),
            };
            if let Some(expected_root) = &root_name {
                anyhow::ensure!(
                    expected_root == &current_root,
                    "GitHub archive contains multiple top-level roots"
                );
            } else {
                root_name = Some(current_root.clone());
            }

            let relative = path
                .strip_prefix(Path::new(&current_root))
                .unwrap_or_else(|_| Path::new(""));
            let target = temp_dir.path().join(&path);
            let size = entry.header().size()?;
            anyhow::ensure!(
                extracted_bytes
                    .checked_add(size)
                    .is_some_and(|total| total <= MAX_EXTRACTED_BYTES),
                "GitHub archive expands beyond the size limit"
            );
            extracted_bytes += size;
            if entry_type.is_dir() {
                fs::create_dir_all(&target)?;
            } else if entry_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut output = File::create(&target)?;
                let copied = copy_limited(&mut entry, &mut output, size)?;
                anyhow::ensure!(
                    copied == size,
                    "GitHub archive file size did not match its tar header"
                );
            } else {
                rejected_entries.push(relative.to_path_buf());
            }
        }

        let root_name = root_name.ok_or_else(|| anyhow::anyhow!("GitHub archive is empty"))?;
        let root = temp_dir.path().join(root_name);
        Ok(Self {
            temp_dir: Some(temp_dir),
            root,
            rejected_entries,
        })
    }

    /// `fetch_blocking` with a content-addressed on-disk cache.
    ///
    /// Floating references (branches, `HEAD`) resolve to a commit SHA first
    /// with one lightweight API request, so cache entries are immutable and
    /// cannot go stale. When resolution fails for a non-rate-limit reason the
    /// fetch proceeds without caching (previous behavior); a rate-limited
    /// resolution stops like any other GitHub rate limit.
    pub fn fetch_blocking_cached(
        client: &crate::client::BlockingClient,
        owner: &str,
        repo: &str,
        reference: &str,
        archive_base: &str,
        cache_root: &Path,
    ) -> anyhow::Result<Self> {
        let Some(commit_sha) = resolve_commit_sha(client, owner, repo, reference)? else {
            return Self::fetch_blocking(client, owner, repo, reference, archive_base);
        };
        let cache_key = format!("{owner}__{repo}__{commit_sha}");
        let cache_dir = cache_root.join(&cache_key);
        if cache_dir.is_dir() {
            tracing::debug!(target: "holon::github_archive", archive = %cache_key, "github_archive_cache_hit");
            return Ok(Self::from_cache_root(cache_dir));
        }
        let archive = Self::fetch_blocking(client, owner, repo, &commit_sha, archive_base)?;
        let archive = archive.move_into_cache(&cache_dir)?;
        evict_archive_cache(cache_root);
        Ok(archive)
    }

    fn from_cache_root(root: PathBuf) -> Self {
        Self {
            temp_dir: None,
            root,
            rejected_entries: Vec::new(),
        }
    }

    /// Move a freshly extracted archive into its cache slot. A concurrent
    /// writer for the same immutable key wins the rename; a loser discards
    /// its copy and shares the installed entry.
    fn move_into_cache(self, cache_dir: &Path) -> anyhow::Result<Self> {
        let rejected_entries = self.rejected_entries.clone();
        if let Some(parent) = cache_dir.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create archive cache root {}", parent.display())
            })?;
        }
        match fs::rename(&self.root, cache_dir) {
            Ok(()) => {}
            Err(rename_error) if cache_dir.is_dir() => {
                tracing::debug!(
                    target: "holon::github_archive",
                    cache = %cache_dir.display(),
                    error = %rename_error,
                    "github_archive_cache_race: keeping existing entry"
                );
            }
            Err(_) => {
                // Cross-device rename: copy the extracted tree, then swap in.
                let staging =
                    cache_dir.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
                if let Err(error) = copy_dir_recursive(&self.root, &staging) {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(error.context(format!(
                        "failed to stage archive cache entry {}",
                        cache_dir.display()
                    )));
                }
                match fs::rename(&staging, cache_dir) {
                    Ok(()) => {}
                    Err(_) if cache_dir.is_dir() => {
                        let _ = fs::remove_dir_all(&staging);
                    }
                    Err(error) => {
                        let _ = fs::remove_dir_all(&staging);
                        return Err(anyhow::Error::new(error).context(format!(
                            "failed to install archive cache entry {}",
                            cache_dir.display()
                        )));
                    }
                }
            }
        }
        Ok(Self {
            temp_dir: None,
            root: cache_dir.to_path_buf(),
            rejected_entries,
        })
    }
}

fn archive_url(
    archive_base: &str,
    owner: &str,
    repo: &str,
    reference: &str,
) -> anyhow::Result<Url> {
    let mut url = Url::parse(archive_base)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "invalid GitHub archive base"
    );
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid GitHub archive base"))?;
        segments
            .clear()
            .push(owner)
            .push(repo)
            .push("tar.gz")
            .push(reference);
    }
    Ok(url)
}

fn rest_archive_url(owner: &str, repo: &str, reference: &str) -> anyhow::Result<String> {
    let mut url = Url::parse("https://api.github.com")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid GitHub REST base"))?;
        segments
            .clear()
            .push("repos")
            .push(owner)
            .push(repo)
            .push("tarball")
            .push(reference);
    }
    Ok(url.into())
}

fn checked_archive_path(path: &Path) -> anyhow::Result<PathBuf> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("GitHub archive path is not valid UTF-8"))?;
    anyhow::ensure!(!value.contains('\0'), "GitHub archive path contains NUL");
    anyhow::ensure!(
        !value.contains('\\'),
        "GitHub archive path contains backslash"
    );
    anyhow::ensure!(!value.starts_with('/'), "GitHub archive path is absolute");
    anyhow::ensure!(
        !(value.len() >= 2
            && value.as_bytes()[1] == b':'
            && value.as_bytes()[0].is_ascii_alphabetic()),
        "GitHub archive path contains a Windows drive"
    );

    let mut normalized = PathBuf::new();
    let value = value.strip_suffix('/').unwrap_or(value);
    anyhow::ensure!(!value.is_empty(), "GitHub archive path is empty");
    for component in value.split('/') {
        anyhow::ensure!(
            !component.is_empty() && component != "." && component != "..",
            "GitHub archive path contains an unsafe component"
        );
        normalized.push(component);
    }
    anyhow::ensure!(
        normalized
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "GitHub archive path is not relative"
    );
    Ok(normalized)
}

fn checked_directory_path(path: &str) -> anyhow::Result<PathBuf> {
    if path.is_empty() || path == "." {
        return Ok(PathBuf::new());
    }
    checked_archive_path(Path::new(path))
}

fn copy_limited(reader: &mut impl Read, writer: &mut impl Write, limit: u64) -> io::Result<u64> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut copied = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(copied);
        }
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        if copied.saturating_add(count) > limit {
            return Err(io::Error::other("archive file exceeds extraction limit"));
        }
        writer.write_all(&buffer[..count as usize])?;
        copied += count;
    }
}

fn validate_archive_limits(path: &Path) -> anyhow::Result<()> {
    let file = File::open(path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(decoder);
    let mut entry_count = 0_usize;
    let mut expanded_bytes = 0_u64;

    for entry in archive.entries()?.raw(true) {
        let entry = entry?;
        entry_count += 1;
        anyhow::ensure!(
            entry_count <= MAX_ENTRIES,
            "GitHub archive contains too many entries"
        );
        let size = entry.header().size()?;
        expanded_bytes = expanded_bytes
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("GitHub archive expands beyond the size limit"))?;
        anyhow::ensure!(
            expanded_bytes <= MAX_EXTRACTED_BYTES,
            "GitHub archive expands beyond the size limit"
        );
    }
    Ok(())
}

fn write_blocking_response(
    mut response: reqwest::blocking::Response,
) -> anyhow::Result<NamedTempFile> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        anyhow::bail!("GitHub archive exceeds the compressed size limit");
    }
    let mut output = NamedTempFile::new()?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut written = 0_u64;
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        written += u64::try_from(count).unwrap_or(u64::MAX);
        anyhow::ensure!(
            written <= MAX_ARCHIVE_BYTES,
            "GitHub archive exceeds the compressed size limit"
        );
        output.write_all(&buffer[..count])?;
    }
    output.as_file_mut().flush()?;
    Ok(output)
}

async fn write_async_response(mut response: reqwest::Response) -> anyhow::Result<NamedTempFile> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        anyhow::bail!("GitHub archive exceeds the compressed size limit");
    }
    let mut output = NamedTempFile::new()?;
    let mut written = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        written += u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        anyhow::ensure!(
            written <= MAX_ARCHIVE_BYTES,
            "GitHub archive exceeds the compressed size limit"
        );
        output.write_all(&chunk)?;
    }
    output.as_file_mut().flush()?;
    Ok(output)
}

async fn unpack_async_response(response: reqwest::Response) -> anyhow::Result<GitHubArchive> {
    let compressed = write_async_response(response).await?;
    tokio::task::spawn_blocking(move || {
        let result = GitHubArchive::from_compressed_file(compressed.path());
        drop(compressed);
        result
    })
    .await?
}

fn http_failure(status: u16, headers: &reqwest::header::HeaderMap) -> ArchiveHttpFailure {
    let retry_after = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    ArchiveHttpFailure {
        status,
        retry_after,
    }
}

async fn async_http_error(
    client: &crate::client::AsyncClient,
    response: reqwest::Response,
) -> anyhow::Error {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let failure = client.response_error(response).await;
    if let Some(rate_limit) = failure.rate_limit {
        anyhow::Error::new(rate_limit)
    } else {
        anyhow::Error::new(http_failure(status, &headers))
    }
}

fn blocking_http_error(
    client: &crate::client::BlockingClient,
    response: reqwest::blocking::Response,
) -> anyhow::Error {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let failure = client.response_error(response);
    if let Some(rate_limit) = failure.rate_limit {
        anyhow::Error::new(rate_limit)
    } else {
        anyhow::Error::new(http_failure(status, &headers))
    }
}

fn parse_retry_after(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(seconds) = value.trim().parse::<i64>() {
        return Some(Utc::now() + ChronoDuration::seconds(seconds.max(0)));
    }
    DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

pub fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ArchiveHttpFailure>()
            .is_some_and(|failure| failure.status == 404)
    })
}

pub fn is_directory_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<ArchiveDirectoryNotFound>())
}

/// Resolve a Git reference (branch, tag, `HEAD`, or short SHA) to a full
/// commit SHA using one GitHub API request. Returns `Ok(None)` when the
/// reference cannot be resolved so callers can fall back to an uncached
/// fetch instead of failing the install.
fn resolve_commit_sha(
    client: &crate::client::BlockingClient,
    owner: &str,
    repo: &str,
    reference: &str,
) -> anyhow::Result<Option<String>> {
    let normalized = reference.trim();
    if normalized.len() == 40 && normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(Some(normalized.to_ascii_lowercase()));
    }
    let url = format!("https://api.github.com/repos/{owner}/{repo}/commits/{normalized}");
    let api_url = client.api_url(&url)?;
    let response = client.get(&api_url).timeout(SHA_RESOLVE_TIMEOUT).send()?;
    if !response.status().is_success() {
        let error = blocking_http_error(client, response);
        // A rate-limited resolution must stop like any other GitHub rate
        // limit; unrelated failures fall back to an uncached fetch.
        if error.downcast_ref::<crate::client::RateLimit>().is_some() {
            return Err(error);
        }
        tracing::debug!(
            target: "holon::github_archive",
            error = %format!("{error:#}"),
            "github archive sha resolution failed; fetching without cache"
        );
        return Ok(None);
    }
    let payload: JsonValue = response.json()?;
    Ok(payload["sha"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|sha| sha.to_ascii_lowercase()))
}

fn evict_archive_cache(cache_root: &Path) {
    let entries = match fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut total: u64 = 0;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata.modified().unwrap_or_else(|_| SystemTime::now());
        let size = directory_size(&path).unwrap_or(0);
        total = total.saturating_add(size);
        candidates.push((modified, size, path));
    }
    if total <= MAX_ARCHIVE_CACHE_BYTES {
        return;
    }
    candidates.sort_by_key(|(modified, _, _)| *modified);
    let cutoff = SystemTime::now()
        .checked_sub(ARCHIVE_CACHE_MIN_AGE)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for (modified, size, path) in candidates {
        if total <= MAX_ARCHIVE_CACHE_BYTES {
            break;
        }
        if modified >= cutoff {
            // Never evict young entries: an in-flight install may still be
            // reading from them.
            break;
        }
        tracing::debug!(target: "holon::github_archive", cache = %path.display(), "evicting github archive cache entry");
        if fs::remove_dir_all(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
