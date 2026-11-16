//! Application re-exports and validation of CLI examples.
pub use holon_context::tools::{tool_sections, tool_sections_with_context, ToolPromptContext};

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use clap::Parser;
    use holon_context::tools::AGENT_INVOCATION_GUIDANCE;

    #[test]
    fn test_parent_progress_example_parses_as_bounded_agent_tail() {
        let content = AGENT_INVOCATION_GUIDANCE;
        let examples = content
            .split('`')
            .filter(|text| text.starts_with("holon tail "))
            .collect::<Vec<_>>();
        assert_eq!(examples.len(), 2, "snapshot and bounded follow examples");
        for (example, follow) in examples.into_iter().zip([false, true]) {
            let cli = crate::cli::Cli::try_parse_from(example.split_whitespace())
                .expect("progress example must match the public CLI");
            let crate::cli::Commands::Tail { options, agent } = cli.command else {
                panic!("progress example must use agent tail");
            };
            assert_eq!(agent.as_deref(), Some("<agent_id>"));
            assert!(options.include_progress);
            assert_eq!(options.follow, follow);
            assert!(options.cursor.is_none());
            assert_eq!(options.limit, 20);
            assert_eq!(options.max_events, 100);
            assert_eq!(options.max_bytes, 65536);
            if follow {
                assert_eq!(options.timeout_ms, 5000);
            }
        }
    }
}
