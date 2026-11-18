use super::*;

fn sample(instant: &str, timezone: &str) -> RuntimeTime {
    RuntimeTime::from_config(instant.parse().unwrap(), Some(timezone), None).unwrap()
}

#[test]
fn all_supported_timezones_fit_initial_time_reservation() {
    for timezone in chrono_tz::TZ_VARIANTS {
        for instant in ["1900-01-01T00:00:00Z", "2026-10-08T15:59:59Z"] {
            let rendered = RuntimeTime::new(instant.parse().unwrap(), timezone).render();
            // Runtime's text estimate uses four Unicode scalars per token.
            assert!(rendered.chars().count() < RUNTIME_TIME_RESERVED_TOKENS * 4);
        }
    }
}

#[test]
fn local_midnight_and_year_boundaries() {
    assert_eq!(
        sample("2025-12-31T16:00:00.999Z", "Asia/Shanghai").current_time(),
        "2026-01-01T00:00:00+08:00"
    );
    assert_eq!(
        sample("2026-01-01T00:00:00Z", "America/Los_Angeles").current_time(),
        "2025-12-31T16:00:00-08:00"
    );
}

#[test]
fn los_angeles_dst_transitions() {
    for (instant, expected) in [
        ("2026-03-08T09:59:59Z", "2026-03-08T01:59:59-08:00"),
        ("2026-03-08T10:00:00Z", "2026-03-08T03:00:00-07:00"),
        ("2026-11-01T08:59:59Z", "2026-11-01T01:59:59-07:00"),
        ("2026-11-01T09:00:00Z", "2026-11-01T01:00:00-08:00"),
    ] {
        assert_eq!(
            sample(instant, "America/Los_Angeles").current_time(),
            expected
        );
    }
}

#[test]
fn precedence_and_invalid_explicit_settings() {
    assert_eq!(resolve_timezone(None, None).unwrap(), chrono_tz::UTC);
    assert_eq!(
        resolve_timezone(None, Some("Asia/Shanghai")).unwrap(),
        chrono_tz::Asia::Shanghai
    );
    assert_eq!(
        resolve_timezone(Some("America/Los_Angeles"), Some("Asia/Shanghai")).unwrap(),
        chrono_tz::America::Los_Angeles
    );
    for invalid in ["", "local", "+08:00", "Mars/Olympus", " UTC "] {
        assert!(parse_timezone(invalid).is_err());
        assert!(resolve_timezone(Some("UTC"), Some(invalid)).is_err());
    }
}

#[test]
fn render_only_exposes_local_clock_and_timezone() {
    let time = sample("2026-01-01T00:00:00Z", "Asia/Shanghai");
    let rendered = time.render();
    assert!(
        rendered.starts_with("current_time: 2026-01-01T08:00:00+08:00\ntimezone: Asia/Shanghai\n")
    );
    assert!(!rendered.contains("2026-01-01T00:00:00"));
    assert!(rendered.contains("not a message or report date"));
    assert!(rendered.contains("latest runtime time block takes precedence"));
}
