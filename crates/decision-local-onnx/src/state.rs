//! Binding-decision state rendering and entity-presence marking.
//!
//! `render_binding_state` produces the B-slot form the local decision model
//! was trained on: history is rendered into the candidate work-item block
//! while `[上下文历史]` stays （无）. `mark_entities` reproduces the
//! entity-presence marking (shared entities in «..», input-only in ‹..›).
//! Both must stay byte-compatible with the training corpus renderer.

use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

pub const INPUT_MARKER: &str = "[新输入]";

const MAX_HISTORY: usize = 3;
const CLIP_HISTORY: usize = 400;
const CLIP_INPUT: usize = 400;
const CLIP_GOAL: usize = 300;
const CLIP_BLOCKER: usize = 200;

fn clip(text: &str, limit: usize) -> String {
    let trimmed = text.trim().replace('\r', "");
    if trimmed.chars().count() <= limit {
        return trimmed;
    }
    let mut clipped: String = trimmed.chars().take(limit - 1).collect();
    clipped.push('…');
    clipped
}

/// Render the binding-decision state in the training corpus B-slot form.
pub fn render_binding_state(kind: &str, history: &[&str], text: &str) -> String {
    let history: Vec<String> = history
        .iter()
        .map(|entry| clip(entry, CLIP_HISTORY))
        .take(MAX_HISTORY)
        .collect();
    let mut lines = vec![
        format!("[输入类型] {kind}"),
        String::new(),
        "[上下文历史]".to_string(),
        "（无）".to_string(),
    ];
    if let Some(first) = history.first() {
        lines.push(String::new());
        lines.push("[候选工作项]".to_string());
        lines.push(format!("目标: {}", clip(first, CLIP_GOAL)));
        if history.len() > 1 {
            let joined = history[1..].join(" / ");
            lines.push(format!("阻塞: {}", clip(&joined, CLIP_BLOCKER)));
        }
    }
    lines.push(String::new());
    lines.push(INPUT_MARKER.to_string());
    lines.push(clip(text, CLIP_INPUT));
    lines.join("\n")
}

fn entity_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"work_[0-9a-f]{14,16}|#\d{1,6}|\d{4,6}").expect("entity regex"))
}

fn is_year(raw: &str) -> bool {
    !raw.starts_with('#')
        && !raw.starts_with('w')
        && raw.len() == 4
        && (raw.starts_with("19") || raw.starts_with("20"))
}

fn entity_key(raw: &str) -> String {
    if raw.starts_with("work_") {
        raw.to_string()
    } else if let Some(stripped) = raw.strip_prefix('#') {
        stripped.to_string()
    } else {
        raw.to_string()
    }
}

/// Ordered unique entity keys: issue/pr numbers, bare 4-6 digit ids, work ids.
/// Bare digit runs must not be preceded by `#`, a digit, `.`, `,` or an ASCII
/// letter, and must not be followed by a digit (ports the Python lookarounds).
pub fn extract_entities(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for mat in entity_regex().find_iter(text) {
        let raw = mat.as_str();
        if !raw.starts_with('#') && !raw.starts_with("work_") {
            let prev_excluded = text[..mat.start()].chars().next_back().is_some_and(|c| {
                c == '#' || c == '.' || c == ',' || c.is_ascii_digit() || c.is_ascii_alphabetic()
            });
            let next_digit = text[mat.end()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
            if prev_excluded || next_digit {
                continue;
            }
        }
        if is_year(raw) {
            continue;
        }
        let key = entity_key(raw);
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    keys
}

/// Mark shared entities «..» symmetrically and input-only entities ‹..› in
/// the input section. Must run on the fully rendered (already clipped) state.
pub fn mark_entities(state: &str) -> String {
    let cut = match state.rfind(INPUT_MARKER) {
        Some(index) => index + INPUT_MARKER.len(),
        None => return state.to_string(),
    };
    let ctx_keys: HashSet<String> = extract_entities(&state[..cut]).into_iter().collect();
    let input_keys: HashSet<String> = extract_entities(&state[cut..]).into_iter().collect();
    let shared: HashSet<String> = ctx_keys.intersection(&input_keys).cloned().collect();
    let fresh: HashSet<String> = input_keys.difference(&ctx_keys).cloned().collect();
    if shared.is_empty() && fresh.is_empty() {
        return state.to_string();
    }
    let mut output = String::with_capacity(state.len());
    let mut last = 0;
    for mat in entity_regex().find_iter(state) {
        let raw = mat.as_str();
        let key = entity_key(raw);
        let marked = if is_year(raw) {
            None
        } else if shared.contains(&key) {
            Some(format!("«{raw}»"))
        } else if fresh.contains(&key) && mat.start() >= cut {
            Some(format!("‹{raw}›"))
        } else {
            None
        };
        if let Some(marked) = marked {
            output.push_str(&state[last..mat.start()]);
            output.push_str(&marked);
            last = mat.end();
        }
    }
    output.push_str(&state[last..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_b_slot_state() {
        let state = render_binding_state("operator_input", &["目标A", "阻塞B"], "继续");
        let expected = "[输入类型] operator_input\n\n[上下文历史]\n（无）\n\n[候选工作项]\n目标: 目标A\n阻塞: 阻塞B\n\n[新输入]\n继续";
        assert_eq!(state, expected);
    }

    #[test]
    fn renders_empty_history_without_candidate_block() {
        let state = render_binding_state("operator_input", &[], "新任务");
        assert_eq!(
            state,
            "[输入类型] operator_input\n\n[上下文历史]\n（无）\n\n[新输入]\n新任务"
        );
    }

    #[test]
    fn clips_long_history_and_input() {
        let long = "x".repeat(500);
        let state = render_binding_state("operator_input", &[&long], &long);
        assert!(state.contains('…'));
        assert!(state.starts_with("[输入类型] operator_input\n"));
    }

    #[test]
    fn extracts_ids_numbers_and_work_ids() {
        let keys = extract_entities("issue #3163 与 work_abcdef1234567890 以及 123456");
        assert_eq!(
            keys,
            vec![
                "3163".to_string(),
                "work_abcdef1234567890".to_string(),
                "123456".to_string()
            ]
        );
    }

    #[test]
    fn skips_years_and_bounded_digits() {
        assert!(extract_entities("2024 年发布").is_empty());
        assert!(extract_entities("abc1234").is_empty());
        assert_eq!(extract_entities("看1234"), vec!["1234".to_string()]);
    }

    #[test]
    fn marks_shared_and_fresh_entities() {
        let state = "上下文提到 #42\n[新输入]\n处理 #42 和 #99";
        let marked = mark_entities(state);
        assert!(marked.contains("«#42»"));
        assert!(marked.contains("‹#99›"));
    }
}
