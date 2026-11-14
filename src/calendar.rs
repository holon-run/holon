pub use holon_domain::calendar::*;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn normalizes_five_field_expression_and_uses_timezone() {
        let schedule = CalendarSchedule::new("30 9 * * 1-5", "America/Los_Angeles").unwrap();
        assert_eq!(schedule.expression, "0 30 9 * * 1-5");
        let after = Utc.with_ymd_and_hms(2026, 1, 2, 18, 0, 0).unwrap();
        let occurrence = schedule.next_after(after).unwrap();
        assert_eq!(occurrence.local, "2026-01-05T09:30:00-08:00");
        assert_eq!(
            occurrence.utc,
            Utc.with_ymd_and_hms(2026, 1, 5, 17, 30, 0).unwrap()
        );
    }

    #[test]
    fn rejects_non_five_field_cron_and_unknown_timezone() {
        assert!(CalendarSchedule::new("* * * *", "UTC").is_err());
        assert!(CalendarSchedule::new("0 0 1 * * *", "UTC").is_err());
        assert!(CalendarSchedule::new("0 0 * * *", "Mars/Olympus").is_err());
    }

    #[test]
    fn rejects_sub_minute_rules() {
        let schedule = CalendarSchedule::new("* * * * *", "UTC").unwrap();
        let occurrence = schedule
            .next_after(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .unwrap();
        assert_eq!(occurrence.utc.second(), 0);
    }
}
