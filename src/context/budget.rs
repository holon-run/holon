//! Token budgeting, estimation, and section truncation helpers.

use crate::prompt::PromptSection;

/// Estimate token count for a prompt section (section header + content).
pub(super) fn estimate_section_tokens(section: &PromptSection) -> usize {
    estimate_text_tokens(&format!("[{}]\n{}", section.name, section.content))
}

/// Estimate token count for a text string using a 4-chars-per-token heuristic.
pub(super) fn estimate_text_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.div_ceil(4).max(1)
}

/// Reserve a portion of the total budget for the current input section.
pub(super) fn reserve_current_input_budget(total_budget: usize) -> usize {
    total_budget.min(256)
}

/// Truncate text to fit within a token budget, optionally appending a truncation
/// notice. Uses binary search to find the maximum character count that fits.
pub(super) fn truncate_section_content(
    prefix: &str,
    text: &str,
    budget: usize,
    truncation_notice: Option<&str>,
) -> String {
    let full = format!("{prefix}{text}");
    if estimate_text_tokens(&full) <= budget {
        return full;
    }

    let suffix = format!("...{}", truncation_notice.unwrap_or(""));
    let prefix_only = prefix.trim_end().to_string();
    if estimate_text_tokens(&(prefix.to_string() + &suffix)) > budget {
        return prefix_only;
    }

    let chars = text.chars().collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate = format!(
            "{prefix}{}{}",
            chars[..mid].iter().collect::<String>(),
            suffix
        );
        if estimate_text_tokens(&candidate) <= budget {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }

    format!(
        "{prefix}{}{}",
        chars[..low].iter().collect::<String>(),
        suffix
    )
}

/// Fit a prompt section within a token budget, truncating content if necessary.
/// Returns `None` if the section cannot fit.
pub(super) fn fit_section_to_budget(
    section: PromptSection,
    budget: usize,
) -> Option<PromptSection> {
    if budget == 0 {
        return None;
    }

    if estimate_section_tokens(&section) <= budget {
        return Some(section);
    }

    let section_header_budget = estimate_text_tokens(&format!("[{}]\n", section.name));
    if budget <= section_header_budget {
        return None;
    }

    let truncated_content = truncate_section_content(
        "",
        &section.content,
        budget.saturating_sub(section_header_budget),
        Some("\n[truncated for budget]"),
    );
    let fitted = PromptSection {
        content: truncated_content,
        ..section
    };
    if fitted.content.trim().is_empty() || estimate_section_tokens(&fitted) > budget {
        None
    } else {
        Some(fitted)
    }
}
