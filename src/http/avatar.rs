use axum::extract::RawQuery;
use sha2::{Digest, Sha256};

use super::*;
use crate::avatar::{self as artwork, Expression};

mod cache;
pub(crate) use cache::AvatarResources;

const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
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
    fn parse(agent_id: &str, query: &str) -> Result<Self, (StatusCode, Json<Value>)> {
        let mut parameters = BTreeMap::new();
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if !matches!(name.as_ref(), "version" | "expression" | "format" | "size")
                || parameters
                    .insert(name.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err(bad_request("unknown or repeated avatar parameter"));
            }
        }
        let parameter =
            |name: &str, default| parameters.get(name).map(String::as_str).unwrap_or(default);
        if parameter("version", artwork::VERSION) != artwork::VERSION {
            return Err(bad_request("unsupported avatar version"));
        }
        let expression = parameter("expression", "neutral")
            .parse()
            .map_err(|_| bad_request("unsupported avatar expression"))?;
        let format = match parameter("format", "png") {
            "png" => Format::Png,
            "svg" => Format::Svg,
            _ => return Err(bad_request("unsupported avatar format")),
        };
        let size = if format == Format::Svg {
            if parameters.contains_key("size") {
                return Err(bad_request("SVG avatars do not accept size"));
            }
            0
        } else {
            match parameter("size", "128") {
                "32" => 32,
                "64" => 64,
                "128" => 128,
                "256" => 256,
                "512" => 512,
                _ => return Err(bad_request("unsupported PNG avatar size")),
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

pub(super) async fn get(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<AxumResponse, AxumResponse> {
    // Even a cache hit or conditional read must revalidate this session/target.
    authorize_remote_access(&headers, &state)
        .map_err(|error| authentication_error_response(error).into_response())?;
    crate::host_registry::validate_agent_id_format(&agent_id)
        .map_err(|error| bad_request(error.to_string()).into_response())?;
    let host = state.host.clone();
    let target = agent_id.clone();
    tokio::task::spawn_blocking(move || host.local_agent_resource_identity(&target))
        .await
        .map_err(|error| error_response(error.into()).into_response())?
        .map_err(|error| agent_access_error(error).into_response())?;
    let key = Key::parse(&agent_id, query.as_deref().unwrap_or_default())
        .map_err(IntoResponse::into_response)?;
    let image = state.avatar_resources.get(key).await.map_err(|error| {
        if matches!(error, cache::BuildError::Failed) {
            return error_response(anyhow!("avatar generation failed")).into_response();
        }
        let mut response =
            service_unavailable("avatar generation capacity exhausted").into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        response
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

#[cfg(test)]
#[cfg(holon_lib_test_shard = "c")]
mod tests;
