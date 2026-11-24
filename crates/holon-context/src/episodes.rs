//! Select archived context episodes and render bounded anchor previews.
use holon_domain::{
    AgentState, ContextConfig, ContextEpisodeRecord, MessageEnvelope, TodoItemState, TurnRecord,
    WorkItemRecord, WorkItemRefStatus,
};
use holon_system::utf8::truncate_text;

use crate::budget::estimate_text_tokens;
use crate::render::{body_preview, enum_label, section};
use crate::PromptSection;

#[derive(Debug, Clone)]
struct EpisodeSelectionAnchor<'a> {
    current_work_item_id: Option<&'a str>,
    objective: Option<&'a str>,
    work_summary: Option<&'a str>,
    working_set_files: &'a [String],
    pending_followups: &'a [String],
    waiting_on: &'a [String],
    query_text: String,
}

pub fn build_relevant_episode_memory_section(
    episodes: &[impl std::borrow::Borrow<ContextEpisodeRecord>],
    _agent: &AgentState,
    current_work_item: Option<&WorkItemRecord>,
    current_message: &MessageEnvelope,
    config: &ContextConfig,
    budget: usize,
    recent_turn_window_start: Option<u64>,
) -> Option<PromptSection> {
    if episodes.is_empty() || config.max_relevant_episodes == 0 || budget == 0 {
        return None;
    }

    let work_ref_values = current_work_item
        .map(|item| {
            item.work_refs
                .iter()
                .filter(|work_ref| work_ref.status == WorkItemRefStatus::Active)
                .map(|work_ref| work_ref.ref_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let working_set_files = current_work_item
        .map(|item| {
            item.work_refs
                .iter()
                .filter(|work_ref| work_ref.status == WorkItemRefStatus::Active)
                .filter(|work_ref| work_ref.kind == holon_domain::WorkItemRefKind::File)
                .map(|work_ref| work_ref.ref_id.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let pending_followups = current_work_item
        .map(|item| {
            item.todo_list
                .iter()
                .filter(|todo| todo.state != TodoItemState::Completed)
                .map(|todo| todo.text.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let waiting_on = current_work_item
        .and_then(|item| item.blocked_by.clone().map(|blocked| vec![blocked]))
        .unwrap_or_default();
    let query_text = format!(
        "{}\n{}\n{}",
        body_preview(&current_message.body),
        work_ref_values.join("\n"),
        current_work_item
            .map(|item| item.objective.as_str())
            .unwrap_or_default()
    );
    let anchor = EpisodeSelectionAnchor {
        current_work_item_id: current_work_item.map(|item| item.id.as_str()),
        objective: current_work_item.map(|item| item.objective.as_str()),
        work_summary: None,
        working_set_files: &working_set_files,
        pending_followups: &pending_followups,
        waiting_on: &waiting_on,
        query_text,
    };

    let episode_count = episodes.len();
    let mut ranked = episodes
        .iter()
        .map(std::borrow::Borrow::borrow)
        .enumerate()
        .filter(|(_, episode)| {
            episode_is_before_recent_turn_window(episode, recent_turn_window_start)
        })
        .map(|(index, episode)| {
            let recency_index = episode_count.saturating_sub(index + 1);
            (
                episode_relevance_score(episode, &anchor, recency_index),
                index,
                episode,
            )
        })
        .filter(|(score, _, _)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.2.finalized_at.cmp(&left.2.finalized_at))
            .then_with(|| right.1.cmp(&left.1))
    });

    let section_overhead = estimate_text_tokens("Archived episode anchors:");
    let mut remaining = budget.saturating_sub(section_overhead);
    let mut blocks = Vec::new();
    for (_, _, episode) in ranked.into_iter().take(config.max_relevant_episodes) {
        let block = render_episode_anchor_block(episode);
        let cost = estimate_text_tokens(&block);
        if cost > remaining && !blocks.is_empty() {
            break;
        }
        remaining = remaining.saturating_sub(cost);
        blocks.push(block);
    }

    if blocks.is_empty() {
        return None;
    }

    Some(section(
        "relevant_episode_memory",
        format!("Archived episode anchors:\n{}", blocks.join("\n")),
    ))
}

pub(crate) fn recent_turn_window_start<'a>(
    turn_records: impl Iterator<Item = &'a TurnRecord>,
) -> Option<u64> {
    turn_records
        .filter_map(|turn| (turn.turn_index != 0).then_some(turn.turn_index))
        .min()
}

fn episode_is_before_recent_turn_window(
    episode: &ContextEpisodeRecord,
    recent_turn_window_start: Option<u64>,
) -> bool {
    recent_turn_window_start
        .map(|start| episode.end_turn_index < start)
        .unwrap_or(true)
}

fn render_episode_anchor_block(episode: &ContextEpisodeRecord) -> String {
    let mut lines = vec![format!("- episode_ref: episode:{}", episode.id)];
    lines.push(format!(
        "  - turns: {}-{}",
        episode.start_turn_index, episode.end_turn_index
    ));
    lines.push(format!(
        "  - boundary: {}",
        enum_label(&episode.boundary_reason)
    ));
    if let Some(work_item_id) = episode.current_work_item_id.as_deref() {
        lines.push(format!("  - work_item_ref: work_item:{work_item_id}"));
    }
    if !episode.source_refs.is_empty() {
        lines.push(format!(
            "  - retrieval_refs: {}",
            episode
                .source_refs
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.source_turn_ids.is_empty() {
        lines.push(format!(
            "  - provenance_turn_ids: {}",
            episode
                .source_turn_ids
                .iter()
                .take(8)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(objective) = episode.objective.as_deref() {
        lines.push(format!(
            "  - objective_preview: {}",
            truncate_text(objective, 180)
        ));
    }
    if let Some(work_summary) = episode.work_summary.as_deref() {
        lines.push(format!(
            "  - work_summary_preview: {}",
            truncate_text(work_summary, 180)
        ));
    }
    if !episode.scope_hints.is_empty() {
        lines.push(format!(
            "  - scope_hints_preview: {}",
            episode
                .scope_hints
                .iter()
                .take(3)
                .map(|hint| truncate_text(hint, 120))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if let Some(generated_by) = episode.generated_by.as_ref() {
        lines.push(format!(
            "  - generated_by: {} for {}",
            generated_by.component,
            enum_label(&generated_by.reason)
        ));
    }
    if !episode.working_set_files.is_empty() {
        lines.push(format!(
            "  - files: {}",
            episode
                .working_set_files
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !episode.carry_forward.is_empty() {
        lines.push(format!(
            "  - followups_preview: {}",
            episode
                .carry_forward
                .iter()
                .take(3)
                .map(|item| truncate_text(item, 120))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !episode.waiting_on.is_empty() {
        lines.push(format!(
            "  - waiting_on_preview: {}",
            episode
                .waiting_on
                .iter()
                .take(3)
                .map(|item| truncate_text(item, 120))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.push(
        "  - retrieval_hint: use MemoryGet with episode_ref or retrieval_refs for exact evidence"
            .to_string(),
    );
    lines.join("\n")
}

fn episode_relevance_score(
    episode: &ContextEpisodeRecord,
    anchor: &EpisodeSelectionAnchor<'_>,
    recency_index: usize,
) -> usize {
    let mut score = anchor
        .current_work_item_id
        .filter(|id| episode.current_work_item_id.as_deref() == Some(*id))
        .map(|_| 120)
        .unwrap_or(0);

    if normalized_option_eq(episode.objective.as_deref(), anchor.objective) {
        score += 80;
    }
    if normalized_option_eq(episode.work_summary.as_deref(), anchor.work_summary) {
        score += 50;
    }

    score += overlap_count(&episode.working_set_files, anchor.working_set_files) * 20;
    score += overlap_count(&episode.carry_forward, anchor.pending_followups) * 16;
    score += overlap_count(&episode.waiting_on, anchor.waiting_on) * 16;

    if episode
        .work_summary
        .as_deref()
        .is_some_and(|summary| text_matches_query(summary, &anchor.query_text))
        || episode
            .objective
            .as_deref()
            .is_some_and(|objective| text_matches_query(objective, &anchor.query_text))
        || episode
            .source_refs
            .iter()
            .any(|source_ref| text_matches_query(source_ref, &anchor.query_text))
    {
        score += 18;
    }
    score + config_recent_bonus(recency_index)
}

fn config_recent_bonus(recency_index: usize) -> usize {
    16usize.saturating_sub(recency_index)
}

fn normalized_option_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => normalize_text(left) == normalize_text(right),
        _ => false,
    }
}

fn overlap_count(left: &[String], right: &[String]) -> usize {
    left.iter()
        .filter(|value| {
            let normalized = normalize_text(value);
            right
                .iter()
                .any(|candidate| normalize_text(candidate) == normalized)
        })
        .count()
}

fn text_matches_query(text: &str, query: &str) -> bool {
    let query_terms = tokenize_significant_terms(query);
    if query_terms.is_empty() {
        return false;
    }
    let haystack = normalize_text(text);
    query_terms
        .iter()
        .any(|term| haystack.contains(term.as_str()))
}

fn tokenize_significant_terms(text: &str) -> Vec<String> {
    normalize_text(text)
        .split_whitespace()
        .filter(|term| term.len() >= 4)
        .map(ToString::to_string)
        .collect()
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
}
