//! Binding-decision state rendering, entity-presence marking, and the
//! question-tail id assembly shared with the trainer's encoder.
//!
//! `render_binding_state` produces the B-slot form the local decision model
//! was trained on: history is rendered into the candidate work-item block
//! while `[上下文历史]` stays （无）. `mark_entities` reproduces the
//! entity-presence marking (shared entities in «..», input-only in ‹..›).
//! Both must stay byte-compatible with the training corpus renderer.

use regex::{Match, Regex};
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

/// Whether a regex hit is an entity candidate under the prev/next-char
/// guards the Python lookarounds `(?<![#\d.,A-Za-z])` and `(?!\d)` enforce
/// for bare digit runs; `#N` and `work_*` ids carry their own prefix and
/// are always candidates. Extraction and marking share this check so they
/// can never disagree about what counts as an entity.
fn is_candidate_match(text: &str, mat: &Match<'_>) -> bool {
    let raw = mat.as_str();
    if raw.starts_with('#') || raw.starts_with("work_") {
        return true;
    }
    let prev_excluded = text[..mat.start()].chars().next_back().is_some_and(|c| {
        c == '#' || c == '.' || c == ',' || c.is_ascii_digit() || c.is_ascii_alphabetic()
    });
    let next_digit = text[mat.end()..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit());
    !(prev_excluded || next_digit)
}

/// Ordered unique entity keys: issue/pr numbers, bare 4-6 digit ids, work ids.
/// Bare digit runs must not be preceded by `#`, a digit, `.`, `,` or an ASCII
/// letter, and must not be followed by a digit (ports the Python lookarounds).
pub fn extract_entities(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for mat in entity_regex().find_iter(text) {
        let raw = mat.as_str();
        if !is_candidate_match(text, &mat) || is_year(raw) {
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
/// The re-scan applies the same lookaround guards as `extract_entities` so
/// marking and extraction agree on what counts as an entity.
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
        if !is_candidate_match(state, &mat) || is_year(raw) {
            continue;
        }
        let key = entity_key(raw);
        let marked = if shared.contains(&key) {
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

/// Assemble `[CLS] + state + tail + [SEP]` ids under the question-tail
/// contract: the tail is never truncated and the state head is trimmed
/// from the front so the most recent context survives. Mirrors the
/// trainer's `encode_decision` budget math.
pub(crate) fn assemble_question_tail(
    cls: i64,
    sep: i64,
    state_ids: &[i64],
    tail_ids: &[i64],
    max_length: usize,
) -> Result<Vec<i64>, &'static str> {
    // The floor of 8 leaves a minimum viable state head behind [CLS]/[SEP]
    // and the tail, matching the Python trainer's budget floor.
    let budget = max_length.saturating_sub(tail_ids.len() + 2);
    if budget < 8 {
        return Err("tail too long for max_length");
    }
    let keep = state_ids.len().min(budget);
    let mut ids = Vec::with_capacity(keep + tail_ids.len() + 2);
    ids.push(cls);
    ids.extend_from_slice(&state_ids[state_ids.len() - keep..]);
    ids.extend_from_slice(tail_ids);
    ids.push(sep);
    Ok(ids)
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

    #[test]
    fn extraction_rejects_version_like_and_digit_prefix_runs() {
        assert!(extract_entities("v1.3201").is_empty());
        assert!(extract_entities("编号 12345678").is_empty());
        assert_eq!(
            extract_entities("分支 v1.3201 与 #3201"),
            vec!["3201".to_string()]
        );
    }

    #[test]
    fn marking_skips_lookaround_excluded_runs() {
        let state = "上下文讨论 #3201\n[新输入]\n继续处理 #3201，见分支 v1.3201";
        let marked = mark_entities(state);
        assert_eq!(
            marked,
            "上下文讨论 «#3201»\n[新输入]\n继续处理 «#3201»，见分支 v1.3201"
        );
    }

    #[test]
    fn marking_skips_digit_prefix_of_longer_runs() {
        let state = "任务 #123456 进行中\n[新输入]\n编号 #123456 与 12345678 对应";
        let marked = mark_entities(state);
        assert!(marked.contains("«#123456»"));
        assert!(!marked.contains("«123456»"));
    }

    #[test]
    fn assembles_question_tail_ids_with_front_truncation() {
        let state_ids: Vec<i64> = (10..30).collect();
        let tail_ids = [40_i64, 41, 42];
        // budget = 20 - 3 - 2 = 15: keep the 15 most recent state ids.
        let ids = assemble_question_tail(1, 2, &state_ids, &tail_ids, 20).expect("ids");
        let mut expected = vec![1];
        expected.extend_from_slice(&state_ids[5..]);
        expected.extend_from_slice(&tail_ids);
        expected.push(2);
        assert_eq!(ids, expected);
        assert_eq!(ids.len(), 20);
    }

    #[test]
    fn rejects_tail_that_starves_the_state_head() {
        let tail_ids = [7_i64; 11];
        // budget = 20 - 11 - 2 = 7 < 8.
        assert!(assemble_question_tail(1, 2, &[10, 11], &tail_ids, 20).is_err());
    }
}
