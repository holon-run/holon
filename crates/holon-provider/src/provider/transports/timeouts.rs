use std::{env, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpTimeouts {
    pub connect: Duration,
    pub request: Duration,
    pub text_headers: Duration,
    pub body: Duration,
    pub idle: Duration,
    pub total: Option<Duration>,
}

impl HttpTimeouts {
    fn from_values(http: Option<u64>, body: Option<u64>, idle: Option<u64>) -> Self {
        let http = http.filter(|value| *value > 0);
        Self {
            connect: Duration::from_secs(30),
            request: Duration::from_secs(http.unwrap_or(300)),
            text_headers: Duration::from_secs(http.unwrap_or(90)),
            body: Duration::from_secs(body.filter(|value| *value > 0).or(http).unwrap_or(300)),
            idle: Duration::from_millis(idle.filter(|value| *value > 0).unwrap_or(300_000)),
            total: http.map(Duration::from_secs),
        }
    }
}

pub(super) fn http_timeouts() -> HttpTimeouts {
    #[cfg(any(test, feature = "runtime-tests"))]
    if let Ok(timeouts) = HTTP_TIMEOUTS_OVERRIDE.try_with(|timeouts| *timeouts) {
        return timeouts;
    }
    let value = |name| env::var(name).ok().and_then(|value| value.parse().ok());
    HttpTimeouts::from_values(
        value("HOLON_PROVIDER_HTTP_TIMEOUT_SECS"),
        value("HOLON_PROVIDER_RESPONSE_BODY_TIMEOUT_SECS"),
        value("HOLON_PROVIDER_STREAM_IDLE_TIMEOUT_MS"),
    )
}

#[cfg(any(test, feature = "runtime-tests"))]
tokio::task_local! {
    pub static HTTP_TIMEOUTS_OVERRIDE: HttpTimeouts;
}

#[cfg(test)]
mod tests;
