use std::collections::BTreeMap;

use axum::{
    body::Bytes,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response as AxumResponse},
};
use sha2::{Digest, Sha256};

pub mod artwork;
mod cache;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;

use artwork::Expression;
pub use cache::AvatarResources;

#[derive(Debug)]
pub enum AvatarResponseError {
    InvalidQuery(&'static str),
    Busy,
    Failed,
}

pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const RENDERER_REVISION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Format {
    Png,
    Svg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    version: &'static str,
    recipe: usize,
    expression: Expression,
    format: Format,
    size: u32,
    revision: u8,
}

impl Key {
    fn parse(agent_id: &str, query: &str) -> Result<Self, &'static str> {
        let mut parameters = BTreeMap::new();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if !matches!(name.as_ref(), "version" | "expression" | "format" | "size")
                || parameters
                    .insert(name.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err("unknown or repeated avatar parameter");
            }
        }
        let parameter =
            |name: &str, default| parameters.get(name).map(String::as_str).unwrap_or(default);
        if parameter("version", artwork::VERSION) != artwork::VERSION {
            return Err("unsupported avatar version");
        }
        let expression = parameter("expression", "neutral")
            .parse()
            .map_err(|_| "unsupported avatar expression")?;
        let format = match parameter("format", "png") {
            "png" => Format::Png,
            "svg" => Format::Svg,
            _ => return Err("unsupported avatar format"),
        };
        let size = if format == Format::Svg {
            if parameters.contains_key("size") {
                return Err("SVG avatars do not accept size");
            }
            0
        } else {
            match parameter("size", "128") {
                "32" => 32,
                "64" => 64,
                "128" => 128,
                "256" => 256,
                "512" => 512,
                _ => return Err("unsupported PNG avatar size"),
            }
        };
        Ok(Self {
            version: artwork::VERSION,
            recipe: artwork::recipe_index(agent_id),
            expression,
            format,
            size,
            revision: RENDERER_REVISION,
        })
    }
}

#[derive(Debug)]
struct Image {
    bytes: Bytes,
    etag: String,
}

impl Image {
    fn new(bytes: Vec<u8>) -> Result<Self, cache::BuildError> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(cache::BuildError::Failed);
        }
        Ok(Self {
            etag: format!("\"{:x}\"", Sha256::digest(&bytes)),
            bytes: bytes.into(),
        })
    }
}

impl AvatarResources {
    pub async fn respond(
        &self,
        agent_id: &str,
        query: &str,
        headers: &HeaderMap,
    ) -> Result<AxumResponse, AvatarResponseError> {
        let key = Key::parse(agent_id, query).map_err(AvatarResponseError::InvalidQuery)?;
        let image = self.get(key).await.map_err(|error| match error {
            cache::BuildError::Failed => AvatarResponseError::Failed,
            cache::BuildError::Overloaded | cache::BuildError::Cancelled => {
                AvatarResponseError::Busy
            }
        })?;
        let not_modified = headers.get_all(IF_NONE_MATCH).iter().any(|header| {
            header.to_str().is_ok_and(|value| {
                value.split(',').any(|tag| {
                    let tag = tag.trim();
                    tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == image.etag
                })
            })
        });
        let mut response = if not_modified {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            image.bytes.clone().into_response()
        };
        let headers = response.headers_mut();
        headers.insert(ETAG, HeaderValue::from_str(&image.etag).expect("hex ETag"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        if !not_modified {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static(match key.format {
                    Format::Png => "image/png",
                    Format::Svg => "image/svg+xml",
                }),
            );
        }
        Ok(response)
    }
}
