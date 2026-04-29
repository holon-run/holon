use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct CodexCliCredential {
    pub access_token: String,
    pub account_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct AuthDotJson {
    #[serde(default)]
    tokens: Option<TokenData>,
    #[serde(default)]
    last_refresh: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    access_token: String,
    #[allow(dead_code)]
    refresh_token: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    id_token: Option<Value>,
}

pub fn load_codex_cli_credential(codex_home: &Path) -> Result<CodexCliCredential> {
    let home = canonical_or_original(codex_home);
    if let Ok(auth) = load_auth_from_file(&home) {
        return auth_to_credential(auth, "file");
    }
    if cfg!(target_os = "macos") {
        if let Ok(auth) = load_auth_from_macos_keychain(&home) {
            return auth_to_credential(auth, "keychain");
        }
    }
    Err(anyhow!(
        "no Codex CLI credentials found in {} or the local keychain; run `codex login` first",
        home.display()
    ))
}

fn load_auth_from_file(codex_home: &Path) -> Result<AuthDotJson> {
    let path = auth_file_path(codex_home);
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_auth_from_macos_keychain(codex_home: &Path) -> Result<AuthDotJson> {
    let account = compute_codex_keychain_account(codex_home);
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Codex Auth",
            "-a",
            &account,
            "-w",
        ])
        .output()
        .context("failed to invoke macOS security tool")?;
    if !output.status.success() {
        return Err(anyhow!(
            "macOS keychain does not contain a Codex Auth entry for account {account}"
        ));
    }
    let secret = String::from_utf8(output.stdout).context("Codex keychain entry was not UTF-8")?;
    serde_json::from_str(secret.trim()).context("failed to parse Codex keychain auth record")
}

fn auth_to_credential(auth: AuthDotJson, source: &str) -> Result<CodexCliCredential> {
    let tokens = auth
        .tokens
        .ok_or_else(|| anyhow!("Codex auth record did not contain OAuth tokens"))?;
    let account_id = tokens
        .account_id
        .or_else(|| {
            tokens
                .id_token
                .as_ref()
                .and_then(extract_account_id_from_id_token)
        })
        .or_else(|| extract_account_id_from_access_token(&tokens.access_token))
        .ok_or_else(|| anyhow!("failed to resolve Codex account id from stored credentials"))?;
    let expires_at = parse_jwt_expiration(&tokens.access_token)
        .ok()
        .flatten()
        .or_else(|| auth.last_refresh.map(|ts| ts + chrono::Duration::hours(1)));

    Ok(CodexCliCredential {
        access_token: tokens.access_token,
        account_id,
        expires_at,
        source: source.to_string(),
    })
}

fn auth_file_path(codex_home: &Path) -> PathBuf {
    codex_home.join("auth.json")
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn compute_codex_keychain_account(codex_home: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(codex_home.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    format!("cli|{}", &hex[..16])
}

fn extract_account_id_from_access_token(token: &str) -> Option<String> {
    let payload = decode_jwt_payload(token).ok()?;
    extract_account_id_from_payload(&payload)
}

fn extract_account_id_from_id_token(id_token: &Value) -> Option<String> {
    match id_token {
        Value::String(jwt) => decode_jwt_payload(jwt)
            .ok()
            .and_then(|payload| extract_account_id_from_payload(&payload)),
        Value::Object(payload) => extract_account_id_from_payload(&Value::Object(payload.clone())),
        _ => None,
    }
}

fn extract_account_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("chatgpt_account_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            payload
                .get("https://api.openai.com/auth")
                .and_then(|value| value.get("chatgpt_account_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn parse_jwt_expiration(token: &str) -> Result<Option<DateTime<Utc>>> {
    let payload = decode_jwt_payload(token)?;
    Ok(payload
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|exp| DateTime::<Utc>::from_timestamp(exp, 0)))
}

fn decode_jwt_payload(token: &str) -> Result<Value> {
    let mut parts = token.split('.');
    let (_header, payload, _sig) = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(sig))
            if !header.is_empty() && !payload.is_empty() && !sig.is_empty() =>
        {
            (header, payload, sig)
        }
        _ => return Err(anyhow!("invalid JWT format")),
    };
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .context("failed to decode JWT payload")?;
    serde_json::from_slice(&payload).context("failed to parse JWT payload")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;

    fn encode_segment(value: serde_json::Value) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    }

    fn make_token(payload: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            encode_segment(json!({"alg": "none"})),
            encode_segment(payload),
            encode_segment(json!("sig"))
        )
    }

    #[test]
    fn extracts_account_id_from_nested_access_claims() {
        let token = make_token(json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_nested"
            }
        }));

        assert_eq!(
            extract_account_id_from_access_token(&token).as_deref(),
            Some("acct_nested")
        );
    }

    #[test]
    fn extracts_account_id_from_chatgpt_account_id_claim() {
        let token = make_token(json!({
            "chatgpt_account_id": "acct_chatgpt"
        }));

        assert_eq!(
            extract_account_id_from_access_token(&token).as_deref(),
            Some("acct_chatgpt")
        );
    }

    #[test]
    fn auth_to_credential_uses_id_token_object_fallback() {
        let credential = auth_to_credential(
            AuthDotJson {
                tokens: Some(TokenData {
                    access_token: make_token(json!({
                        "exp": 1_900_000_000
                    })),
                    refresh_token: "refresh".to_string(),
                    account_id: None,
                    id_token: Some(json!({
                        "chatgpt_account_id": "acct_id_token"
                    })),
                }),
                last_refresh: None,
            },
            "file",
        )
        .expect("id_token object fallback should resolve account id");

        assert_eq!(credential.account_id, "acct_id_token");
        assert_eq!(credential.source, "file");
    }

    #[test]
    fn keychain_account_matches_cli_prefix_shape() {
        let account = compute_codex_keychain_account(std::path::Path::new("/tmp/codex-home"));
        assert!(account.starts_with("cli|"));
        assert_eq!(account.len(), 20);
    }
}
