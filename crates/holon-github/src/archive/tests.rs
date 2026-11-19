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
        let encoder = flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::fast());
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

fn blocking_client(auth: Option<&str>, base: &str) -> crate::client::BlockingClient {
    let auth = auth.map_or_else(crate::client::GitHubAuth::anonymous, |token| {
        crate::client::GitHubAuth::from_token(
            Some(token.to_owned()),
            crate::client::AuthSource::Configured,
        )
        .unwrap()
    });
    crate::client::BlockingClient::new(reqwest::blocking::Client::new(), auth, base).unwrap()
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
        let temp_root = snapshot.temp_dir.as_ref().unwrap().path();
        assert!(!temp_root.join("pax_global_header").exists());
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
    let temp_root = snapshot.temp_dir.as_ref().unwrap().path();
    assert_eq!(fs::read_dir(temp_root).unwrap().count(), 1);
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
    let error = GitHubArchive::fetch_blocking(&client, "owner", "repo", "main", &base).unwrap_err();
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
    let error = GitHubArchive::fetch_blocking(&client, "owner", "repo", "main", &base).unwrap_err();
    let failure = error.downcast_ref::<ArchiveHttpFailure>().unwrap();
    assert_eq!(failure.status, 429);
    assert!(failure.retry_after.unwrap() >= before + ChronoDuration::seconds(6));
    handle.join().unwrap();
}

#[tokio::test]
async fn async_fetch_uses_same_archive_contract() {
    let body = archive(&[("root/file", tar::EntryType::Regular, b"ok")]);
    let (base, handle) = serve(vec![(200, vec![], body)]);
    let client = crate::client::AsyncClient::new(
        reqwest::Client::new(),
        crate::client::GitHubAuth::anonymous(),
        &base,
    )
    .unwrap();
    let archive = GitHubArchive::fetch(&client, "owner", "repo", "main", &base)
        .await
        .unwrap();
    assert!(archive.root().join("file").is_file());
    handle.join().unwrap();
}
