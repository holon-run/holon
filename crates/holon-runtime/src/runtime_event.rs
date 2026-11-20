pub use holon_domain::runtime_event::*;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::RUNTIME_EVENT_CONTRACT_VERSION;

    #[test]
    fn web_gui_supports_current_runtime_event_contract() {
        let session_events = include_str!("../../../web-gui/app/src/runtime/session-events.ts");
        let expected =
            format!("const RUNTIME_EVENT_CONTRACT_VERSION = {RUNTIME_EVENT_CONTRACT_VERSION};");

        assert!(
            session_events.contains(&expected),
            "Web GUI runtime event contract must match backend version {RUNTIME_EVENT_CONTRACT_VERSION}"
        );
    }
}
