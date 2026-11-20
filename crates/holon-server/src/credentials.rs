use axum::http::{
    header::{AUTHORIZATION, COOKIE},
    HeaderMap,
};

pub const SESSION_COOKIE_NAME: &str = "holon_session";

pub fn bearer_session_credential(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let mut parts = value.split_whitespace();
    if !parts
        .next()
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("Bearer"))
    {
        return None;
    }
    let token = parts.next().filter(|token| !token.is_empty())?;
    if parts.next().is_some() {
        return None;
    }
    Some(token.to_string())
}

pub fn cookie_session_credential(headers: &HeaderMap) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == SESSION_COOKIE_NAME && !value.is_empty()).then(|| value.to_string())
            })
        })
}

pub fn session_credential(headers: &HeaderMap) -> Option<String> {
    bearer_session_credential(headers).or_else(|| cookie_session_credential(headers))
}
