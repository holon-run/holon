use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) const DEFAULT_LOCAL_ONNX_PRESET: &str = "jev-selector-q4f16";
const MODEL_REPOSITORY: &str = "onnx-community/open-jev-deberta-v3-large-ONNX";
const MODEL_REVISION: &str = "7c79f25b5ac496089f448a969c801872ad59d31c";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LocalOnnxPresetFile {
    pub name: &'static str,
    pub local_name: &'static str,
    pub remote_path: &'static str,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LocalOnnxPresetManifest {
    pub preset: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub files: &'static [LocalOnnxPresetFile],
}

const PRESET_FILES: &[LocalOnnxPresetFile] = &[
    LocalOnnxPresetFile {
        name: "model",
        local_name: "model_q4f16.onnx",
        remote_path: "onnx/model_q4f16.onnx",
        sha256: "f9f014e96682fa0a9ac3c7b3b41786f58a2996dbff890b8ed18eed3869cc495d",
    },
    LocalOnnxPresetFile {
        name: "model_data",
        local_name: "model_q4f16.onnx_data",
        remote_path: "onnx/model_q4f16.onnx_data",
        sha256: "89bd9c644c621e3d79fbd1de6bb03e622c38e1dd42e11ab8acece8a236c3481f",
    },
    LocalOnnxPresetFile {
        name: "tokenizer",
        local_name: "tokenizer.json",
        remote_path: "tokenizer.json",
        sha256: "cd119378b0160677b7a1e561ba29ada83918c1d420b326516a585382e83d9d39",
    },
    LocalOnnxPresetFile {
        name: "config",
        local_name: "open_jev_config.json",
        remote_path: "open_jev_config.json",
        sha256: "128c6b453bda8f477a186c6e13a303f2665c75330daab4ced322f84767bb5799",
    },
];

pub(crate) const DEFAULT_LOCAL_ONNX_MANIFEST: LocalOnnxPresetManifest = LocalOnnxPresetManifest {
    preset: DEFAULT_LOCAL_ONNX_PRESET,
    repository: MODEL_REPOSITORY,
    revision: MODEL_REVISION,
    files: PRESET_FILES,
};

pub(crate) fn managed_local_onnx_dir(home_dir: &Path, preset: &str) -> Result<PathBuf> {
    validate_preset_name(preset)?;
    Ok(home_dir
        .join("models")
        .join("decision")
        .join("local-onnx")
        .join(preset))
}

pub(crate) fn preset_manifest(preset: &str) -> Result<&'static LocalOnnxPresetManifest> {
    if preset == DEFAULT_LOCAL_ONNX_PRESET {
        Ok(&DEFAULT_LOCAL_ONNX_MANIFEST)
    } else {
        Err(anyhow!("unknown local ONNX preset: {preset}"))
    }
}

pub(crate) fn local_onnx_preset_status(
    home_dir: &Path,
    preset: &str,
) -> Result<LocalOnnxPresetStatus> {
    let manifest = preset_manifest(preset)?;
    let directory = managed_local_onnx_dir(home_dir, preset)?;
    let temporary = directory.with_extension("download");
    let (status_directory, phase) = if directory.is_dir() {
        (directory.clone(), "ready")
    } else if temporary.is_dir() {
        (temporary.clone(), "partial")
    } else {
        (directory.clone(), "missing")
    };
    let files = manifest
        .files
        .iter()
        .map(|file| {
            let path = status_directory.join(file.local_name);
            let present = path.is_file();
            let verified = present && sha256_file(&path).is_ok_and(|hash| hash == file.sha256);
            LocalOnnxPresetFileStatus {
                name: file.name,
                path,
                sha256: file.sha256,
                present,
                verified,
                bytes: if present {
                    fs::metadata(&status_directory.join(file.local_name))
                        .map(|metadata| metadata.len())
                        .unwrap_or_default()
                } else {
                    0
                },
            }
        })
        .collect::<Vec<_>>();
    let complete = phase == "ready" && files.iter().all(|file| file.verified);
    let phase = if complete {
        "complete"
    } else if phase == "ready" {
        "corrupt"
    } else {
        phase
    };
    Ok(LocalOnnxPresetStatus {
        preset: manifest.preset,
        directory,
        phase,
        complete,
        downloaded_bytes: files.iter().map(|file| file.bytes).sum(),
        bytes_total: None,
        retryable: !complete,
        cancellable: phase == "partial",
        files,
    })
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalOnnxPresetStatus {
    pub preset: &'static str,
    pub directory: PathBuf,
    pub phase: &'static str,
    pub complete: bool,
    pub downloaded_bytes: u64,
    pub bytes_total: Option<u64>,
    pub retryable: bool,
    pub cancellable: bool,
    pub files: Vec<LocalOnnxPresetFileStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalOnnxPresetFileStatus {
    pub name: &'static str,
    pub path: PathBuf,
    pub sha256: &'static str,
    pub present: bool,
    pub verified: bool,
    pub bytes: u64,
}

pub(crate) fn download_local_onnx_preset(
    home_dir: &Path,
    preset: &str,
) -> Result<LocalOnnxPresetStatus> {
    let manifest = preset_manifest(preset)?;
    let destination = managed_local_onnx_dir(home_dir, preset)?;
    if local_onnx_preset_status(home_dir, preset)?.complete {
        return local_onnx_preset_status(home_dir, preset);
    }

    fs::create_dir_all(destination.parent().context("managed model parent")?)?;
    let temporary = destination.with_extension("download");
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }
    fs::create_dir_all(&temporary)?;

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("build model download client")?;
    for file in manifest.files {
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            manifest.repository, manifest.revision, file.remote_path
        );
        let response = client
            .get(url)
            .send()
            .with_context(|| format!("download {}", file.name))?
            .error_for_status()
            .with_context(|| format!("download {}", file.name))?;
        let target = temporary.join(file.local_name);
        let mut output = File::create(&target)?;
        let mut reader = response;
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
        }
        output.sync_all()?;
        let actual = sha256_file(&target)?;
        anyhow::ensure!(
            actual == file.sha256,
            "{} checksum mismatch: expected {}, got {}",
            file.name,
            file.sha256,
            actual
        );
    }
    let model_metadata = serde_json::to_vec_pretty(&serde_json::json!({
        "model": "model_q4f16.onnx",
        "tokenizer": "tokenizer.json",
        "input_ids": "input_ids",
        "attention_mask": "attention_mask",
        "token_type_ids": "token_type_ids",
        "output": "logits",
        "max_length": 512,
        "temperature": 1.05
    }))?;
    fs::write(temporary.join("holon_model_manifest.json"), model_metadata)?;
    fs::write(
        temporary.join("holon-preset.json"),
        serde_json::to_vec_pretty(manifest)?,
    )?;
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    fs::rename(&temporary, &destination)?;
    local_onnx_preset_status(home_dir, preset)
}

pub(crate) fn cancel_local_onnx_preset(
    home_dir: &Path,
    preset: &str,
) -> Result<LocalOnnxPresetStatus> {
    preset_manifest(preset)?;
    let directory = managed_local_onnx_dir(home_dir, preset)?;
    let temporary = directory.with_extension("download");
    if temporary.is_dir() {
        fs::remove_dir_all(&temporary)?;
    }
    local_onnx_preset_status(home_dir, preset)
}

fn validate_preset_name(preset: &str) -> Result<()> {
    anyhow::ensure!(
        !preset.is_empty()
            && preset
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "invalid local ONNX preset name"
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn preset_status_reports_missing_and_partial_downloads() {
        let home = tempdir().unwrap();
        let missing = local_onnx_preset_status(home.path(), DEFAULT_LOCAL_ONNX_PRESET).unwrap();
        assert_eq!(missing.phase, "missing");
        assert!(missing.retryable);
        assert!(!missing.cancellable);

        let temporary = managed_local_onnx_dir(home.path(), DEFAULT_LOCAL_ONNX_PRESET)
            .unwrap()
            .with_extension("download");
        fs::create_dir_all(&temporary).unwrap();
        fs::write(temporary.join(PRESET_FILES[0].local_name), b"partial").unwrap();
        let partial = local_onnx_preset_status(home.path(), DEFAULT_LOCAL_ONNX_PRESET).unwrap();
        assert_eq!(partial.phase, "partial");
        assert!(partial.cancellable);
        assert!(partial.downloaded_bytes > 0);
    }

    #[test]
    fn cancelling_preset_removes_partial_download() {
        let home = tempdir().unwrap();
        let directory = managed_local_onnx_dir(home.path(), DEFAULT_LOCAL_ONNX_PRESET).unwrap();
        let temporary = directory.with_extension("download");
        fs::create_dir_all(&temporary).unwrap();
        fs::write(temporary.join(PRESET_FILES[0].local_name), b"partial").unwrap();

        let status = cancel_local_onnx_preset(home.path(), DEFAULT_LOCAL_ONNX_PRESET).unwrap();
        assert_eq!(status.phase, "missing");
        assert!(!temporary.exists());
    }
}
