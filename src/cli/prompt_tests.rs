use clap::Parser;
use holon_context::docs::{get_doc_topic, DOCS_TOPICS};

#[test]
fn test_parent_progress_example_parses_as_bounded_agent_tail() {
    let content = get_doc_topic("agents")
        .expect("embedded agents manual")
        .content;
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

#[test]
fn test_holon_docs_cli_parses_topic_and_json_modes() {
    let index = crate::cli::Cli::try_parse_from(["holon", "docs"]).expect("docs index");
    assert!(matches!(
        index.command,
        crate::cli::Commands::Docs {
            topic: None,
            json: false,
            output: crate::cli::DocsOutputFormat::Text,
        }
    ));

    for entry in DOCS_TOPICS {
        let parsed = crate::cli::Cli::try_parse_from(["holon", "docs", entry.slug])
            .expect("each embedded topic slug must parse");
        let crate::cli::Commands::Docs {
            topic,
            json,
            output,
        } = parsed.command
        else {
            panic!("expected Docs command");
        };
        assert_eq!(topic.as_deref(), Some(entry.slug));
        assert!(!json);
        assert_eq!(output, crate::cli::DocsOutputFormat::Text);
    }

    let json_flag =
        crate::cli::Cli::try_parse_from(["holon", "docs", "skills", "--json"]).expect("--json");
    assert!(matches!(
        json_flag.command,
        crate::cli::Commands::Docs {
            topic: Some(ref slug),
            json: true,
            output: crate::cli::DocsOutputFormat::Text,
        } if slug == "skills"
    ));

    let output_json =
        crate::cli::Cli::try_parse_from(["holon", "docs", "models", "--output", "json"])
            .expect("--output json");
    assert!(matches!(
        output_json.command,
        crate::cli::Commands::Docs {
            topic: Some(ref slug),
            json: false,
            output: crate::cli::DocsOutputFormat::Json,
        } if slug == "models"
    ));
}

#[test]
fn test_embedded_docs_referenced_commands_match_public_cli() {
    for sample in [
        "holon commands",
        "holon context",
        "holon docs cli --json",
        "holon skills add ./my-skill",
        "holon skills enable my-skill",
        "holon skills list",
        "holon skills check",
        "holon agent list --parent self",
        "holon agent delete child-1 --yes --wait",
        "holon config providers list",
        "holon config models list",
        "holon config web-providers",
        "holon agent model get",
        "holon agent reset-callback",
        "holon workspace attach /tmp/repo",
        "holon memory-index rebuild",
    ] {
        crate::cli::Cli::try_parse_from(sample.split_whitespace()).unwrap_or_else(|err| {
            panic!("referenced CLI command `{sample}` failed to parse: {err}")
        });
    }

    for topic in DOCS_TOPICS {
        for snippet in topic.content.split('`') {
            if !snippet.starts_with("holon ") || snippet.contains('|') {
                continue;
            }
            let normalized = snippet
                .split_whitespace()
                .filter(|token| !(token.starts_with('[') && token.ends_with(']')))
                .map(|token| {
                    if token.starts_with('<') && token.ends_with('>') {
                        "placeholder"
                    } else {
                        token
                    }
                })
                .collect::<Vec<_>>();
            match crate::cli::Cli::try_parse_from(&normalized) {
                Ok(_) => {}
                Err(err)
                    if matches!(
                        err.kind(),
                        clap::error::ErrorKind::MissingSubcommand
                            | clap::error::ErrorKind::MissingRequiredArgument
                            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    ) =>
                {
                    let mut with_help = normalized.clone();
                    with_help.push("--help");
                    let help_check = crate::cli::Cli::try_parse_from(&with_help);
                    assert!(
                        matches!(
                            help_check,
                            Err(ref help_err)
                                if help_err.kind() == clap::error::ErrorKind::DisplayHelp
                        ),
                        "CLI command family `{snippet}` in docs topic `{}` is invalid: {err}",
                        topic.slug
                    );
                }
                Err(err) => {
                    panic!(
                        "CLI snippet `{snippet}` in docs topic `{}` failed to parse: {err}",
                        topic.slug
                    );
                }
            }
        }
    }
}
