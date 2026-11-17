use super::*;
use holon_context::{render_sections, section, PromptLayout};

fn shared(name: &'static str, content: &str) -> PromptSection {
    let mut value = section(name, PromptStability::Stable, content.into());
    value.layout = PromptLayout::SharedPrefix;
    value
}

fn text(blocks: &[PromptContentBlock]) -> String {
    blocks.iter().map(|block| block.text.as_str()).collect()
}

#[test]
fn shared_boundary_is_not_the_last_stable_section() {
    let system = vec![
        shared("identity", "fixed role"),
        shared("tool_contract", "fixed tools"),
        section("workspace", PromptStability::AgentScoped, "/task-a".into()),
        section(
            "response_language",
            PromptStability::Stable,
            "language footer".into(),
        ),
    ];
    let (blocks, context) = build_prompt_content_blocks(&system, &[]);
    assert_eq!(text(&blocks), render_sections(&system));
    assert!(context.is_empty());
    assert_eq!(
        blocks
            .iter()
            .enumerate()
            .filter_map(|(i, block)| block.cache_breakpoint.then_some(i))
            .collect::<Vec<_>>(),
        vec![1, 3],
    );
    assert!(blocks[1].text.contains("fixed tools"));
    assert!(!blocks[2].cache_breakpoint);
}

#[test]
fn shared_wire_prefix_does_not_depend_on_suffix_or_context() {
    let shared = vec![
        shared("identity", "fixed role"),
        shared("tool_contract", "fixed tools"),
    ];
    let (prefix_blocks, _) = build_prompt_content_blocks(&shared, &[]);
    let expected = text(&prefix_blocks);
    assert_eq!(expected, render_sections(&shared));
    assert!(expected.ends_with("\n\n"));
    for path in ["/task-a/random-root", "/task-b/unrelated-root"] {
        let mut system = shared.clone();
        system.push(section(
            "workspace",
            PromptStability::AgentScoped,
            path.into(),
        ));
        for contexts in [
            vec![],
            vec![section(
                "current_input",
                PromptStability::TurnScoped,
                path.into(),
            )],
            vec![
                section("agent_memory", PromptStability::AgentScoped, path.into()),
                section("current_input", PromptStability::TurnScoped, path.into()),
            ],
        ] {
            let (blocks, context) = build_prompt_content_blocks(&system, &contexts);
            assert_eq!(text(&blocks), render_sections(&system));
            assert_eq!(text(&context), render_sections(&contexts));
            assert_eq!(text(&blocks[..2]), expected);
            assert!(blocks[1].cache_breakpoint);
            assert!(text(&blocks).ends_with(path));
        }
    }
}

#[test]
fn marker_budget_deduplicates_and_never_marks_turn_scoped_content() {
    for prefix in [vec![], vec![shared("core", "shared")]] {
        for suffix in [
            vec![],
            vec![section(
                "footer",
                PromptStability::Stable,
                "fixed but scoped".into(),
            )],
            vec![section(
                "event",
                PromptStability::TurnScoped,
                "dynamic event".into(),
            )],
        ] {
            let mut system = prefix.clone();
            system.extend(suffix);
            for context in [
                vec![],
                vec![section(
                    "stable_context",
                    PromptStability::Stable,
                    "context".into(),
                )],
                vec![
                    section(
                        "memory",
                        PromptStability::AgentScoped,
                        "agent memory".into(),
                    ),
                    section("input", PromptStability::TurnScoped, "new input".into()),
                ],
            ] {
                let (system_blocks, context_blocks) =
                    build_prompt_content_blocks(&system, &context);
                let cacheable = system_blocks
                    .iter()
                    .chain(&context_blocks)
                    .filter(|block| block.stability != PromptStability::TurnScoped)
                    .count();
                let marked = system_blocks
                    .iter()
                    .chain(&context_blocks)
                    .filter(|block| block.cache_breakpoint)
                    .collect::<Vec<_>>();
                assert!(marked.len() <= 2);
                assert_eq!(marked.is_empty(), cacheable == 0);
                assert!(marked
                    .iter()
                    .all(|block| block.stability != PromptStability::TurnScoped));
                assert_eq!(text(&system_blocks), render_sections(&system));
            }
        }
    }
    let sections = vec![shared("core", "shared")];
    let (blocks, _) = build_prompt_content_blocks(&sections, &[]);
    assert_eq!(
        blocks.iter().filter(|block| block.cache_breakpoint).count(),
        1
    );
}

#[test]
fn legacy_stable_sections_are_not_inferred_to_be_shared() {
    let system = vec![
        section(
            "identity",
            PromptStability::Stable,
            "workspace /private/a".into(),
        ),
        section("policy", PromptStability::AgentScoped, "agent rules".into()),
    ];
    let (blocks, _) = build_prompt_content_blocks(&system, &[]);
    assert_eq!(shared_prefix_len(&system), 0);
    assert!(!blocks[0].cache_breakpoint);
    assert!(blocks[1].cache_breakpoint);
    assert_eq!(text(&blocks), render_sections(&system));
}
