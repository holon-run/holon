use std::{
    collections::HashSet,
    fmt,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use flate2::read::GzDecoder;
use reqwest::Url;
use tar::Archive;
use tempfile::{NamedTempFile, TempDir};

pub(crate) const DEFAULT_ARCHIVE_BASE: &str = "https://codeload.github.com";

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub(crate) struct ArchiveHttpFailure {
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
pub(crate) struct GitHubArchive {
    #[allow(dead_code)]
    temp_dir: TempDir,
    root: PathBuf,
    rejected_entries: Vec<PathBuf>,
}

impl GitHubArchive {
    #[cfg(test)]
    pub(crate) fn from_test_root(temp_dir: TempDir) -> Self {
        let root = temp_dir.path().to_path_buf();
        Self {
            temp_dir,
            root,
            rejected_entries: Vec::new(),
        }
    }

    pub(crate) async fn fetch(
        client: &crate::github::AsyncClient,
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

    pub(crate) fn fetch_blocking(
        client: &crate::github::BlockingClient,
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

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn directory(&self, path: &str) -> anyhow::Result<PathBuf> {
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
            temp_dir,
            root,
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
    client: &crate::github::AsyncClient,
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
    client: &crate::github::BlockingClient,
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

pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<ArchiveHttpFailure>()
            .is_some_and(|failure| failure.status == 404)
    })
}

pub(crate) fn is_directory_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<ArchiveDirectoryNotFound>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        net::{TcpListener, TcpStream},
        thread,
    };

    const GLOBAL_PAX: &[u8] = b"13 comment=x\n";

    fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (path, kind, contents) in entries {
                let mut header = tar::Header::new_gnu();
                if header.set_path(path).is_err() {
                    let path = path.as_bytes();
                    assert!(path.len() <= 100);
                    let gnu = header.as_gnu_mut().unwrap();
                    gnu.name.fill(0);
                    gnu.name[..path.len()].copy_from_slice(path);
                }
                header.set_entry_type(*kind);
                header.set_size(contents.len() as u64);
                if kind.is_symlink() {
                    header.set_link_name("ok.txt").unwrap();
                } else if kind.is_hard_link() {
                    header.set_link_name("root/ok.txt").unwrap();
                }
                header.set_cksum();
                builder.append(&header, Cursor::new(*contents)).unwrap();
            }
            let encoder = builder.into_inner().unwrap();
            encoder.finish().unwrap();
        }
        compressed
    }

    fn parse_archive(bytes: Vec<u8>) -> anyhow::Result<GitHubArchive> {
        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        GitHubArchive::from_compressed_file(file.path())
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let record = format!(" {key}={value}\n");
        let mut length = record.len() + 1;
        loop {
            let payload = format!("{length}{record}");
            if payload.len() == length {
                return payload.into_bytes();
            }
            length = payload.len();
        }
    }

    fn blocking_client(auth: Option<&str>, base: &str) -> crate::github::BlockingClient {
        let auth = auth.map_or_else(crate::github::GitHubAuth::anonymous, |token| {
            crate::github::GitHubAuth::from_token(
                Some(token.to_owned()),
                crate::github::AuthSource::Configured,
            )
            .unwrap()
        });
        crate::github::BlockingClient::new(reqwest::blocking::Client::new(), auth, base).unwrap()
    }

    fn serve(
        responses: Vec<(u16, Vec<(&'static str, &'static str)>, Vec<u8>)>,
    ) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut paths = Vec::new();
            for (status, headers, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                paths.push(request.lines().next().unwrap_or_default().to_owned());
                let reason = if status == 200 { "OK" } else { "Error" };
                let mut response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
            paths
        });
        (format!("http://{address}"), handle)
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    #[test]
    fn rejects_unsafe_paths_duplicates_and_multiple_roots() {
        for path in [
            "../escape",
            "/absolute",
            r"windows\\path",
            "C:/drive",
            "nul\0path",
        ] {
            let bytes = archive(&[
                ("root/file", tar::EntryType::Directory, &[]),
                (path, tar::EntryType::Regular, b"x"),
            ]);
            let file = NamedTempFile::new().unwrap();
            fs::write(file.path(), bytes).unwrap();
            assert!(
                GitHubArchive::from_compressed_file(file.path()).is_err(),
                "{path:?}"
            );
        }
        for entries in [
            vec![
                ("root/file", tar::EntryType::Regular, b"x".as_slice()),
                ("root/file", tar::EntryType::Regular, b"y".as_slice()),
            ],
            vec![
                ("root/file", tar::EntryType::Regular, b"x".as_slice()),
                ("other/file", tar::EntryType::Regular, b"y".as_slice()),
            ],
        ] {
            let bytes = archive(&entries);
            let file = NamedTempFile::new().unwrap();
            fs::write(file.path(), bytes).unwrap();
            assert!(GitHubArchive::from_compressed_file(file.path()).is_err());
        }
    }

    #[test]
    fn skips_links_and_special_entries_until_selected() {
        let bytes = archive(&[
            ("root/ok.txt", tar::EntryType::Regular, b"ok"),
            ("root/link", tar::EntryType::Symlink, &[]),
            ("root/fifo", tar::EntryType::Fifo, &[]),
            ("root/other/ok.txt", tar::EntryType::Regular, b"ok"),
        ]);
        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        let archive = GitHubArchive::from_compressed_file(file.path()).unwrap();
        assert!(archive.root().join("ok.txt").is_file());
        assert!(archive.directory("other").is_ok());
        assert!(archive.directory("").is_err());
    }

    #[test]
    fn accepts_global_pax_before_and_between_files() {
        for position in [0, 1] {
            let mut entries = vec![
                ("root/one.txt", tar::EntryType::Regular, b"one".as_slice()),
                ("root/two.txt", tar::EntryType::Regular, b"two".as_slice()),
            ];
            entries.insert(
                position,
                (
                    "pax_global_header",
                    tar::EntryType::XGlobalHeader,
                    GLOBAL_PAX,
                ),
            );
            let snapshot = parse_archive(archive(&entries)).unwrap();
            assert_eq!(snapshot.root().file_name().unwrap(), "root");
            assert_eq!(fs::read(snapshot.root().join("one.txt")).unwrap(), b"one");
            assert_eq!(fs::read(snapshot.root().join("two.txt")).unwrap(), b"two");
            assert_eq!(snapshot.directory("").unwrap(), snapshot.root());
            assert!(snapshot.rejected_entries.is_empty());
            assert!(!snapshot.temp_dir.path().join("pax_global_header").exists());
        }
    }

    #[test]
    fn ignores_global_pax_by_type_without_materializing_metadata() {
        let snapshot = parse_archive(archive(&[
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            ("root/ok.txt", tar::EntryType::XGlobalHeader, GLOBAL_PAX),
            ("root/ok.txt", tar::EntryType::XGlobalHeader, GLOBAL_PAX),
            ("../metadata", tar::EntryType::XGlobalHeader, GLOBAL_PAX),
            ("/metadata", tar::EntryType::XGlobalHeader, GLOBAL_PAX),
            (
                "unusual-metadata",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            (
                "root/nested/metadata",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            ("root/ok.txt", tar::EntryType::Regular, b"ok"),
        ]))
        .unwrap();
        assert_eq!(fs::read(snapshot.root().join("ok.txt")).unwrap(), b"ok");
        assert_eq!(snapshot.root().file_name().unwrap(), "root");
        assert!(snapshot.rejected_entries.is_empty());
        assert_eq!(fs::read_dir(snapshot.temp_dir.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(snapshot.root()).unwrap().count(), 1);
    }

    #[test]
    fn does_not_ignore_regular_files_named_pax_global_header() {
        let snapshot = parse_archive(archive(&[(
            "root/pax_global_header",
            tar::EntryType::Regular,
            b"ordinary content",
        )]))
        .unwrap();
        assert_eq!(
            fs::read(snapshot.root().join("pax_global_header")).unwrap(),
            b"ordinary content"
        );
        let error = parse_archive(archive(&[
            (
                "pax_global_header",
                tar::EntryType::Regular,
                b"ordinary content",
            ),
            ("root/file", tar::EntryType::Regular, b"x"),
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("multiple top-level roots"));
    }

    #[test]
    fn rejects_metadata_only_archives_as_empty() {
        for names in [
            vec!["pax_global_header"],
            vec!["root/metadata", "root/metadata"],
        ] {
            let entries: Vec<_> = names
                .into_iter()
                .map(|name| (name, tar::EntryType::XGlobalHeader, GLOBAL_PAX))
                .collect();
            let error = parse_archive(archive(&entries)).unwrap_err();
            assert_eq!(error.to_string(), "GitHub archive is empty");
        }
    }

    #[test]
    fn global_pax_prefix_preserves_content_validation() {
        for (path, expected) in [
            ("root/file", "contains a duplicate path: root/file"),
            ("other/file", "contains multiple top-level roots"),
            ("root/../escape", "path contains an unsafe component"),
            ("/absolute", "path is absolute"),
        ] {
            let error = parse_archive(archive(&[
                (
                    "pax_global_header",
                    tar::EntryType::XGlobalHeader,
                    GLOBAL_PAX,
                ),
                ("root/file", tar::EntryType::Regular, b"x"),
                (path, tar::EntryType::Regular, b"y"),
            ]))
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{path}: {error}");
        }
    }

    #[test]
    fn global_pax_does_not_hide_links_or_special_entries() {
        let snapshot = parse_archive(archive(&[
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            ("root/ok.txt", tar::EntryType::Regular, b"ok"),
            ("root/link", tar::EntryType::Symlink, &[]),
            ("root/hardlink", tar::EntryType::Link, &[]),
            ("root/fifo", tar::EntryType::Fifo, &[]),
            ("root/char", tar::EntryType::Char, &[]),
            ("root/block", tar::EntryType::Block, &[]),
            ("root/other/ok.txt", tar::EntryType::Regular, b"ok"),
        ]))
        .unwrap();
        assert!(snapshot.directory("other").is_ok());
        assert_eq!(snapshot.rejected_entries.len(), 5);
        for path in ["", "link", "hardlink", "fifo", "char", "block"] {
            let error = snapshot.directory(path).unwrap_err();
            assert_eq!(
                error.to_string(),
                "selected archive directory contains unsupported entries"
            );
        }
        for path in ["link", "hardlink", "fifo", "char", "block"] {
            assert!(!snapshot.root().join(path).exists());
        }
    }

    #[test]
    fn raw_metadata_counts_toward_entry_limit() {
        let metadata = [
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                GLOBAL_PAX,
            ),
            ("PaxHeaders/file", tar::EntryType::XHeader, GLOBAL_PAX),
            (
                "././@LongLink",
                tar::EntryType::GNULongName,
                b"root/file\0".as_slice(),
            ),
        ];
        let mut entries: Vec<_> = metadata.iter().copied().cycle().take(MAX_ENTRIES).collect();
        entries.push(("root/file", tar::EntryType::Regular, b"x"));
        let error = parse_archive(archive(&entries)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub archive contains too many entries"
        );
    }

    #[test]
    fn raw_metadata_counts_toward_expansion_limit() {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(GLOBAL_PAX.len() as u64);
        header.set_cksum();
        builder.append(&header, Cursor::new(GLOBAL_PAX)).unwrap();
        header.set_size(MAX_EXTRACTED_BYTES);
        header.set_cksum();
        // Preflight must reject the cumulative size before reading this omitted payload.
        builder.append(&header, io::empty()).unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        let error = parse_archive(bytes).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub archive expands beyond the size limit"
        );
    }

    #[test]
    fn accepts_safe_gnu_longname_and_local_pax_content_paths() {
        let path = format!("root/{}/file", "segment".repeat(20));
        for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
            let payload = if kind.is_gnu_longname() {
                format!("{path}\0").into_bytes()
            } else {
                pax_record("path", &path)
            };
            let snapshot = parse_archive(archive(&[
                (
                    "pax_global_header",
                    tar::EntryType::XGlobalHeader,
                    GLOBAL_PAX,
                ),
                ("././@LongLink", kind, &payload),
                ("root/placeholder", tar::EntryType::Regular, b"content"),
            ]))
            .unwrap();
            assert_eq!(
                fs::read(snapshot.root().join(path.strip_prefix("root/").unwrap())).unwrap(),
                b"content"
            );
            assert!(!snapshot.root().join("placeholder").exists());
            assert!(snapshot.rejected_entries.is_empty());
        }
    }

    #[test]
    fn rejects_unsafe_gnu_longname_and_local_pax_effective_paths() {
        for kind in [tar::EntryType::GNULongName, tar::EntryType::XHeader] {
            for (path, expected) in [
                ("root/../escape", "path contains an unsafe component"),
                ("/absolute", "path is absolute"),
            ] {
                let payload = if kind.is_gnu_longname() {
                    format!("{path}\0").into_bytes()
                } else {
                    pax_record("path", path)
                };
                let error = parse_archive(archive(&[
                    ("././@LongLink", kind, &payload),
                    ("root/placeholder", tar::EntryType::Regular, b"content"),
                ]))
                .unwrap_err();
                assert!(
                    error.to_string().contains(expected),
                    "{kind:?} {path}: {error}"
                );
            }
        }
    }

    #[test]
    fn cleanup_removes_extracted_snapshot() {
        let bytes = archive(&[("root/file", tar::EntryType::Regular, b"content")]);
        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        let root = {
            let archive = GitHubArchive::from_compressed_file(file.path()).unwrap();
            let root = archive.root().to_path_buf();
            assert!(root.join("file").is_file());
            root
        };
        assert!(!root.exists());
    }

    #[test]
    #[ignore = "requires GitHub network access"]
    fn fixed_github_revisions_expose_agent_templates() {
        let client = blocking_client(None, "https://api.github.com");
        for (repo, revision) in [
            ("holon-test", "34b7e2cc93dc428f8c1b4bf696de6dcfe7373436"),
            ("holon", "fd58e2222c5434bc0be5c75e4f24c889b29b2d1c"),
        ] {
            let snapshot = GitHubArchive::fetch_blocking(
                &client,
                "holon-run",
                repo,
                revision,
                DEFAULT_ARCHIVE_BASE,
            )
            .unwrap_or_else(|error| panic!("{repo}@{revision}: {error:#}"));
            let directory = snapshot.directory("agent_templates").unwrap();
            let entries = fs::read_dir(directory)
                .unwrap()
                .collect::<io::Result<Vec<_>>>()
                .unwrap();
            assert!(!entries.is_empty(), "{repo}@{revision}: empty templates");
        }
    }

    #[test]
    fn authenticated_archive_404_falls_back_to_rest() {
        let body = archive(&[("root/file", tar::EntryType::Regular, b"ok")]);
        let (base, handle) = serve(vec![(404, vec![], Vec::new()), (200, vec![], body)]);
        let client = blocking_client(Some("secret"), &base);
        let archive =
            GitHubArchive::fetch_blocking(&client, "owner", "repo", "heads/main", &base).unwrap();
        assert!(archive.root().join("file").is_file());
        let paths = handle.join().unwrap();
        assert!(paths[0].contains("/owner/repo/tar.gz/heads%2Fmain"));
        assert!(paths[1].contains("/repos/owner/repo/tarball/heads%2Fmain"));
    }

    #[test]
    fn anonymous_archive_404_does_not_fall_back() {
        let (base, handle) = serve(vec![(404, vec![], Vec::new())]);
        let client = blocking_client(None, &base);
        let error =
            GitHubArchive::fetch_blocking(&client, "owner", "repo", "main", &base).unwrap_err();
        assert!(is_not_found(&error));
        assert_eq!(handle.join().unwrap().len(), 1);
    }

    #[test]
    fn retry_after_is_exposed_without_response_body() {
        let (base, handle) = serve(vec![(
            429,
            vec![("Retry-After", "7")],
            b"secret-token".to_vec(),
        )]);
        let client = blocking_client(None, &base);
        let before = Utc::now();
        let error =
            GitHubArchive::fetch_blocking(&client, "owner", "repo", "main", &base).unwrap_err();
        let failure = error.downcast_ref::<ArchiveHttpFailure>().unwrap();
        assert_eq!(failure.status, 429);
        assert!(failure.retry_after.unwrap() >= before + ChronoDuration::seconds(6));
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn async_fetch_uses_same_archive_contract() {
        let body = archive(&[("root/file", tar::EntryType::Regular, b"ok")]);
        let (base, handle) = serve(vec![(200, vec![], body)]);
        let client = crate::github::AsyncClient::new(
            reqwest::Client::new(),
            crate::github::GitHubAuth::anonymous(),
            &base,
        )
        .unwrap();
        let archive = GitHubArchive::fetch(&client, "owner", "repo", "main", &base)
            .await
            .unwrap();
        assert!(archive.root().join("file").is_file());
        handle.join().unwrap();
    }
}
