//! Legacy JSONL append/read/tail/migration helpers.

use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use chrono::Utc;

pub(crate) fn migrate_events_ledger(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if let Some(seq) = read_tail_event_seq(path)? {
        return Ok(seq);
    }

    let timestamp = Utc::now().format("%Y%m%d%H%M%S%3f");
    let tmp_path = path.with_file_name(format!(".events.jsonl.{timestamp}.tmp"));
    let file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut tmp = fs::File::create(&tmp_path)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    let mut max_seq = 0;
    let mut changed = false;

    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(&line)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("event ledger line is not a JSON object"))?;
        match object.get("event_seq").and_then(Value::as_u64) {
            Some(seq) if seq > max_seq => {
                max_seq = seq;
            }
            Some(seq) => {
                anyhow::bail!(
                    "event ledger sequence must be strictly increasing; found {seq} after {max_seq}"
                );
            }
            None => {
                max_seq += 1;
                object.insert("event_seq".to_string(), Value::from(max_seq));
                changed = true;
            }
        }
        writeln!(tmp, "{}", serde_json::to_string(&value)?)?;
    }

    if !changed {
        let _ = fs::remove_file(&tmp_path);
        return Ok(max_seq);
    }

    let backup_path = path.with_file_name(format!("events.jsonl.bak.{timestamp}"));
    fs::copy(path, &backup_path).with_context(|| {
        format!(
            "failed to back up {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace {} with {}",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(max_seq)
}

pub(crate) fn read_tail_event_seq(path: &Path) -> Result<Option<u64>> {
    let Some(value) = read_latest_jsonl_matching::<Value, _>(path, |_| true)? else {
        return Ok(Some(0));
    };
    Ok(value.get("event_seq").and_then(Value::as_u64))
}

pub(crate) fn max_jsonl_u64_field(path: &Path, field: &str) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let mut max_value = None;
    scan_jsonl_reverse::<Value, _>(path, |value| {
        if let Some(sequence) = value.get(field).and_then(Value::as_u64) {
            max_value = Some(sequence);
            return false;
        }
        true
    })?;
    Ok(max_value.unwrap_or(0))
}

pub(crate) fn jsonl_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let line = serde_json::to_string(value)?;
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    Ok(bytes)
}

pub(crate) fn append_jsonl_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

pub(crate) fn read_recent_jsonl<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<Vec<T>> {
    if !path.exists() || limit == 0 {
        return Ok(Vec::new());
    }

    let file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut recent = VecDeque::with_capacity(limit.min(1024));
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        if recent.len() == limit {
            recent.pop_front();
        }
        recent.push_back(line);
    }
    recent
        .into_iter()
        .map(|line| serde_json::from_str::<T>(&line).map_err(Into::into))
        .collect()
}

pub(crate) fn take_recent<T>(mut records_desc: Vec<T>, limit: usize) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    if records_desc.len() > limit {
        records_desc.truncate(limit);
    }
    records_desc.reverse();
    records_desc
}

pub(crate) fn read_jsonl_from<T: DeserializeOwned>(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut lines = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(offset)
        .map(|line| serde_json::from_str::<T>(line))
        .collect::<Result<Vec<_>, _>>()?;

    if lines.len() > limit {
        lines.drain(0..(lines.len() - limit));
    }
    Ok(lines)
}

pub(crate) fn read_latest_jsonl_matching<T, F>(path: &Path, mut matches: F) -> Result<Option<T>>
where
    T: DeserializeOwned,
    F: FnMut(&T) -> bool,
{
    if !path.exists() {
        return Ok(None);
    }

    const CHUNK_SIZE: u64 = 8192;
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = file.seek(SeekFrom::End(0))?;
    let mut prefix = Vec::new();

    while cursor > 0 {
        let read_len = cursor.min(CHUNK_SIZE);
        cursor -= read_len;
        file.seek(SeekFrom::Start(cursor))?;

        let mut chunk = vec![0; read_len as usize];
        file.read_exact(&mut chunk)
            .with_context(|| format!("failed to read {}", path.display()))?;
        chunk.extend_from_slice(&prefix);

        let mut line_end = chunk.len();
        for idx in (0..chunk.len()).rev() {
            if chunk[idx] != b'\n' {
                continue;
            }
            if let Some(record) =
                parse_jsonl_match(&chunk[(idx + 1)..line_end], path, &mut matches)?
            {
                return Ok(Some(record));
            }
            line_end = idx;
        }
        prefix = chunk[..line_end].to_vec();
    }

    parse_jsonl_match(&prefix, path, &mut matches)
}

pub(crate) fn scan_jsonl_reverse<T, F>(path: &Path, mut visit: F) -> Result<()>
where
    T: DeserializeOwned,
    F: FnMut(T) -> bool,
{
    if !path.exists() {
        return Ok(());
    }

    const CHUNK_SIZE: u64 = 8192;
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = file.seek(SeekFrom::End(0))?;
    let mut prefix = Vec::new();

    while cursor > 0 {
        let read_len = cursor.min(CHUNK_SIZE);
        cursor -= read_len;
        file.seek(SeekFrom::Start(cursor))?;

        let mut chunk = vec![0; read_len as usize];
        file.read_exact(&mut chunk)
            .with_context(|| format!("failed to read {}", path.display()))?;
        chunk.extend_from_slice(&prefix);

        let mut line_end = chunk.len();
        for idx in (0..chunk.len()).rev() {
            if chunk[idx] != b'\n' {
                continue;
            }
            if !parse_jsonl_visit(&chunk[(idx + 1)..line_end], path, &mut visit)? {
                return Ok(());
            }
            line_end = idx;
        }
        prefix = chunk[..line_end].to_vec();
    }

    let _ = parse_jsonl_visit(&prefix, path, &mut visit)?;
    Ok(())
}

pub(crate) fn parse_jsonl_visit<T, F>(line: &[u8], path: &Path, visit: &mut F) -> Result<bool>
where
    T: DeserializeOwned,
    F: FnMut(T) -> bool,
{
    let line = std::str::from_utf8(line)
        .with_context(|| format!("failed to decode UTF-8 from {}", path.display()))?;
    if line.trim().is_empty() {
        return Ok(true);
    }
    let record: T = serde_json::from_str(line)
        .with_context(|| format!("failed to decode line from {}", path.display()))?;
    Ok(visit(record))
}

pub(crate) fn parse_jsonl_match<T, F>(
    line: &[u8],
    path: &Path,
    matches: &mut F,
) -> Result<Option<T>>
where
    T: DeserializeOwned,
    F: FnMut(&T) -> bool,
{
    let line = std::str::from_utf8(line)
        .with_context(|| format!("failed to decode UTF-8 from {}", path.display()))?;
    if line.trim().is_empty() {
        return Ok(None);
    }
    let record: T = serde_json::from_str(line)
        .with_context(|| format!("failed to decode line from {}", path.display()))?;
    if matches(&record) {
        Ok(Some(record))
    } else {
        Ok(None)
    }
}
