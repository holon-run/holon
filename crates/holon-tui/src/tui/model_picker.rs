use crate::types::{AgentSummary, ResolvedModelAvailability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelPickerChoice {
    InheritDefault,
    Provider {
        provider: String,
    },
    Model {
        model: String,
        reasoning_effort_options: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPickerRow {
    pub(super) choice: ModelPickerChoice,
    pub(super) title: String,
    pub(super) detail: String,
    pub(super) searchable: String,
    pub(super) available: bool,
}

pub(super) fn model_picker_rows(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
) -> Vec<ModelPickerRow> {
    let Some(agent) = agent else {
        return Vec::new();
    };
    let inherit_row = inherit_default_row(agent);
    let query = filter.trim().to_ascii_lowercase();
    let choice_rows = match provider {
        Some(provider) => provider_model_rows(model_availability, provider),
        None => provider_rows(model_availability),
    };
    if query.is_empty() {
        let mut rows = vec![inherit_row];
        rows.extend(choice_rows);
        return rows;
    }

    let mut rows = vec![inherit_row];
    rows.extend(
        choice_rows
            .into_iter()
            .filter(|row| row.searchable.contains(&query)),
    );
    rows
}

pub(super) fn selected_model_picker_row(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
    selected: usize,
) -> Option<ModelPickerRow> {
    model_picker_rows(agent, model_availability, provider, filter)
        .into_iter()
        .nth(selected)
}

pub(super) fn clamp_model_picker_selection(
    agent: Option<&AgentSummary>,
    model_availability: &[ResolvedModelAvailability],
    provider: Option<&str>,
    filter: &str,
    selected: usize,
) -> usize {
    let len = model_picker_rows(agent, model_availability, provider, filter).len();
    if len == 0 {
        0
    } else {
        selected.min(len - 1)
    }
}

fn inherit_default_row(agent: &AgentSummary) -> ModelPickerRow {
    let default_model = agent.model.runtime_default_model.as_compact_display();
    let current = agent.model.override_model.is_none();
    let title = if current {
        format!("inherit runtime default: {default_model} (current)")
    } else {
        format!("inherit runtime default: {default_model}")
    };
    ModelPickerRow {
        choice: ModelPickerChoice::InheritDefault,
        detail: "clear this agent's model override".into(),
        searchable: format!("inherit default runtime {default_model}").to_ascii_lowercase(),
        title,
        available: true,
    }
}

fn provider_rows(model_availability: &[ResolvedModelAvailability]) -> Vec<ModelPickerRow> {
    let mut providers =
        std::collections::BTreeMap::<String, Vec<&ResolvedModelAvailability>>::new();
    for entry in model_availability {
        if !entry.available {
            continue;
        }
        providers
            .entry(entry.provider.clone())
            .or_default()
            .push(entry);
    }

    providers
        .into_iter()
        .map(|(provider, models)| provider_row(provider, models))
        .collect()
}

fn provider_row(provider: String, models: Vec<&ResolvedModelAvailability>) -> ModelPickerRow {
    let available_count = models.iter().filter(|entry| entry.available).count();
    let discovered_count = models
        .iter()
        .filter(|entry| entry.metadata_source == "remote_discovered")
        .count();
    let first = models.first().copied();
    let transport = first
        .and_then(|entry| entry.transport.as_deref())
        .map(|transport| format!(" transport:{transport}"))
        .unwrap_or_default();
    let source = first
        .and_then(|entry| entry.provider_source.as_deref())
        .map(|source| format!(" provider:{source}"))
        .unwrap_or_default();
    let credential = first
        .and_then(|entry| entry.credential_source.as_deref())
        .map(|source| format!(" credential:{source}"))
        .unwrap_or_default();
    let status = format!(
        "ready: {available_count}/{} models selectable",
        models.len()
    );
    let discovery = if discovered_count > 0 {
        format!(" remote-discovered:{discovered_count}")
    } else {
        String::new()
    };
    ModelPickerRow {
        choice: ModelPickerChoice::Provider {
            provider: provider.clone(),
        },
        title: provider.clone(),
        detail: format!("{status}{transport}{source}{credential}{discovery}"),
        searchable: format!("{provider} {status}{transport}{source}{credential}{discovery}")
            .to_ascii_lowercase(),
        available: true,
    }
}

fn provider_model_rows(
    model_availability: &[ResolvedModelAvailability],
    provider: &str,
) -> Vec<ModelPickerRow> {
    model_availability
        .iter()
        .filter(|entry| entry.provider == provider)
        .filter(|entry| entry.available)
        .map(model_availability_row)
        .collect()
}

fn model_availability_row(entry: &ResolvedModelAvailability) -> ModelPickerRow {
    let status = if entry.available {
        "ready".to_string()
    } else {
        format!(
            "unavailable: {}",
            entry
                .unavailable_reason
                .as_deref()
                .unwrap_or("provider unavailable")
        )
    };
    let reasoning = if supports_reasoning_effort(entry) {
        " reasoning:configurable"
    } else {
        " reasoning:skipped"
    };
    let provider = entry
        .provider_source
        .as_deref()
        .map(|source| format!("{} provider:{source}", entry.provider))
        .unwrap_or_else(|| entry.provider.clone());
    let transport = entry
        .transport
        .as_deref()
        .map(|transport| format!(" transport:{transport}"))
        .unwrap_or_default();
    ModelPickerRow {
        choice: ModelPickerChoice::Model {
            model: entry.model.clone(),
            reasoning_effort_options: entry.policy.reasoning_effort_options.clone(),
        },
        title: format!("{}  {}", entry.model, entry.display_name),
        detail: format!(
            "{status}  {provider}{transport}{reasoning}  source:{}",
            entry.metadata_source
        ),
        searchable: format!(
            "{} {} {} {} {} {}",
            entry.model,
            entry.display_name,
            entry.provider,
            entry.metadata_source,
            status,
            reasoning
        )
        .to_ascii_lowercase(),
        available: entry.available,
    }
}

fn supports_reasoning_effort(entry: &ResolvedModelAvailability) -> bool {
    !entry.policy.reasoning_effort_options.is_empty()
}

#[cfg(test)]
mod tests;
