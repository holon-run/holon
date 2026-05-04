use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use anyhow::{anyhow, Result};
use serde_json::json;
use tokio::net::lookup_host;
use url::Url;

use crate::{tool::ToolError, web::WebFetchConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAccessDecision {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub resolved_ips: Vec<IpAddr>,
    pub allowed_by_host_rule: bool,
}

impl WebAccessDecision {
    pub fn pinned_socket_addrs(&self) -> Vec<SocketAddr> {
        self.resolved_ips
            .iter()
            .map(|ip| SocketAddr::new(*ip, self.port))
            .collect()
    }
}

pub async fn validate_fetch_url(url: &Url, config: &WebFetchConfig) -> Result<WebAccessDecision> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(policy_error(
            "network_denied",
            "WebFetch only allows explicit http or https URLs",
            json!({ "url": url.as_str(), "scheme": url.scheme() }),
            "provide an http or https URL",
        ));
    }

    let host = url
        .host_str()
        .ok_or_else(|| {
            policy_error(
                "network_denied",
                "WebFetch requires a URL with a host",
                json!({ "url": url.as_str() }),
                "provide a URL with a hostname or IP address",
            )
        })?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let port = url.port_or_known_default().ok_or_else(|| {
        policy_error(
            "network_denied",
            "WebFetch requires a URL with a known network port",
            json!({ "url": url.as_str() }),
            "provide an http or https URL with a valid port",
        )
    })?;

    if host_rule_matches(&config.denied_hosts, &host, port) {
        return Err(policy_error(
            "network_denied",
            "WebFetch blocked the host because it matches web.fetch.denied_hosts",
            json!({ "url": url.as_str(), "host": host, "port": port }),
            "choose another URL or update web.fetch.denied_hosts",
        ));
    }

    let allowed_by_host_rule = host_rule_matches(&config.allowed_hosts, &host, port);
    if !config.allowed_hosts.is_empty() && !allowed_by_host_rule {
        return Err(policy_error(
            "network_denied",
            "WebFetch blocked the host because it is not in web.fetch.allowed_hosts",
            json!({ "url": url.as_str(), "host": host, "port": port }),
            "add the host to web.fetch.allowed_hosts or fetch an allowed host",
        ));
    }

    let resolved_ips = resolve_host(&host, port).await?;
    if resolved_ips.iter().any(is_metadata_ip) {
        return Err(policy_error(
            "network_denied",
            "WebFetch blocked a cloud metadata endpoint",
            json!({ "url": url.as_str(), "host": host, "resolved_ips": resolved_ips }),
            "metadata endpoints are not accessible through WebFetch",
        ));
    }

    let private_ips = resolved_ips
        .iter()
        .copied()
        .filter(is_private_or_local_ip)
        .collect::<Vec<_>>();
    if !private_ips.is_empty() && !allowed_by_host_rule {
        return Err(policy_error(
            "network_denied",
            "WebFetch blocked loopback, private, or link-local network access",
            json!({
                "url": url.as_str(),
                "host": host,
                "blocked_ips": private_ips,
            }),
            "explicitly allow a development host through web.fetch.allowed_hosts",
        ));
    }

    Ok(WebAccessDecision {
        url: url.as_str().to_string(),
        host,
        port,
        resolved_ips,
        allowed_by_host_rule,
    })
}

pub(crate) fn timeout(config: &WebFetchConfig) -> Duration {
    Duration::from_secs(config.timeout_seconds.max(1))
}

pub(crate) fn policy_error(
    kind: &'static str,
    message: impl Into<String>,
    details: serde_json::Value,
    recovery_hint: impl Into<String>,
) -> anyhow::Error {
    anyhow::Error::from(
        ToolError::new(kind, message)
            .with_details(details)
            .with_recovery_hint(recovery_hint)
            .with_retryable(false),
    )
}

async fn resolve_host(host: &str, port: u16) -> Result<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let addresses = lookup_host((host, port))
        .await
        .map_err(|error| anyhow!("failed to resolve {host}: {error}"))?
        .map(|address| address.ip())
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(anyhow!("failed to resolve {host}: no addresses returned"));
    }
    Ok(addresses)
}

fn host_rule_matches(rules: &[String], host: &str, port: u16) -> bool {
    rules.iter().any(|rule| {
        let Some(parsed) = parse_host_rule(rule) else {
            return false;
        };
        if !parsed.host.eq_ignore_ascii_case(host) {
            return false;
        }
        parsed.port.is_none_or(|rule_port| rule_port == port)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostRule {
    host: String,
    port: Option<u16>,
}

fn parse_host_rule(rule: &str) -> Option<HostRule> {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = rest[..close].to_ascii_lowercase();
        let suffix = &rest[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            suffix.strip_prefix(':')?.parse::<u16>().ok()
        };
        return Some(HostRule { host, port });
    }

    let colon_count = trimmed.matches(':').count();
    if colon_count == 1 {
        let (host, port) = trimmed.rsplit_once(':')?;
        if let Ok(port) = port.parse::<u16>() {
            return Some(HostRule {
                host: host.to_ascii_lowercase(),
                port: Some(port),
            });
        }
    }

    Some(HostRule {
        host: trimmed.to_ascii_lowercase(),
        port: None,
    })
}

fn is_private_or_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || (ip.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

fn is_metadata_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.octets() == [169, 254, 169, 254] || ip.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_loopback_by_default() {
        let config = WebFetchConfig::default();
        let url = Url::parse("http://127.0.0.1:3000/").unwrap();
        let error = validate_fetch_url(&url, &config).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "network_denied");
    }

    #[tokio::test]
    async fn allowlisted_loopback_is_allowed_for_dev_hosts() {
        let mut config = WebFetchConfig::default();
        config.allowed_hosts = vec!["127.0.0.1:3000".into()];
        let url = Url::parse("http://127.0.0.1:3000/").unwrap();
        let decision = validate_fetch_url(&url, &config).await.unwrap();
        assert!(decision.allowed_by_host_rule);
    }

    #[tokio::test]
    async fn allowlisted_ipv6_loopback_with_port_matches() {
        let mut config = WebFetchConfig::default();
        config.allowed_hosts = vec!["[::1]:5173".into()];
        let url = Url::parse("http://[::1]:5173/").unwrap();
        let decision = validate_fetch_url(&url, &config).await.unwrap();
        assert!(decision.allowed_by_host_rule);
    }

    #[tokio::test]
    async fn metadata_endpoint_stays_blocked_when_host_is_allowlisted() {
        let mut config = WebFetchConfig::default();
        config.allowed_hosts = vec!["169.254.169.254".into()];
        let url = Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
        let error = validate_fetch_url(&url, &config).await.unwrap_err();
        let tool_error = ToolError::from_anyhow(&error);
        assert_eq!(tool_error.kind, "network_denied");
        assert!(tool_error.message.contains("metadata"));
    }
}
