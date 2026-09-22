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
    let complete = manifest.files.iter().all(|file| {
        let path = directory.join(file.local_name);
        path.is_file() && sha256_file(&path).is_ok_and(|hash| hash == file.sha256)
    });
    Ok(LocalOnnxPresetStatus {
        preset: manifest.preset,
        directory: directory.clone(),
        complete,
        files: manifest
            .files
            .iter()
            .map(|file| LocalOnnxPresetFileStatus {
                name: file.name,
                path: directory.join(file.local_name),
                sha256: file.sha256,
                present: directory.join(file.local_name).is_file(),
                verified: directory.join(file.local_name).is_file()
                    && sha256_file(&directory.join(file.local_name))
                        .is_ok_and(|hash| hash == file.sha256),
            })
            .collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalOnnxPresetStatus {
    pub preset: &'static str,
    pub directory: PathBuf,
    pub complete: bool,
    pub files: Vec<LocalOnnxPresetFileStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalOnnxPresetFileStatus {
    pub name: &'static str,
    pub path: PathBuf,
    pub sha256: &'static str,
    pub present: bool,
    pub verified: bool,
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
