//! Locale detection for the control-plane agent creation path.
//!
//! The detected locale only produces a one-time response-language hint for a
//! new agent's first default bootstrap reply. It never changes runtime
//! configuration, and an unmapped locale never fails the command.

use holon_runtime::types::normalize_initial_response_language;

/// Locale environment variables consulted in priority order.
const LOCALE_ENV_VARS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// Detect the creator's locale for a one-time response-language hint.
///
/// The first variable that carries a value wins, matching gettext override
/// semantics: `LC_ALL` overrides `LC_MESSAGES`, which overrides `LANG`. A
/// value that cannot be mapped to a language tag yields no hint instead of
/// failing the command.
pub fn detect_response_language() -> Option<String> {
    let env: Vec<(&str, String)> = LOCALE_ENV_VARS
        .iter()
        .map(|name| (*name, std::env::var(name).unwrap_or_default()))
        .collect();
    detect_response_language_from(&env)
}

fn detect_response_language_from<S: AsRef<str>>(env: &[(&str, S)]) -> Option<String> {
    for name in LOCALE_ENV_VARS {
        let Some(value) = env
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_ref())
        else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        return normalize_initial_response_language(value);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_all_overrides_the_other_variables() {
        assert_eq!(
            detect_response_language_from(&[
                ("LC_ALL", "zh_CN.UTF-8"),
                ("LC_MESSAGES", "en_US.UTF-8"),
                ("LANG", "fr_FR.UTF-8"),
            ]),
            Some("zh-CN".to_string())
        );
    }

    #[test]
    fn falls_back_to_locale_messages_then_lang() {
        assert_eq!(
            detect_response_language_from(&[
                ("LC_MESSAGES", "en_US.UTF-8"),
                ("LANG", "fr_FR.UTF-8"),
            ]),
            Some("en-US".to_string())
        );
        assert_eq!(
            detect_response_language_from(&[("LANG", "en_US.UTF-8")]),
            Some("en-US".to_string())
        );
    }

    #[test]
    fn omits_the_hint_when_the_locale_cannot_be_mapped() {
        assert_eq!(
            detect_response_language_from(&[("LC_ALL", "C"), ("LANG", "zh_CN.UTF-8")]),
            None
        );
        assert_eq!(detect_response_language_from::<&str>(&[]), None);
    }

    #[test]
    fn ignores_empty_locale_variables() {
        assert_eq!(
            detect_response_language_from(&[
                ("LC_ALL", ""),
                ("LC_MESSAGES", "  "),
                ("LANG", "zh_CN.UTF-8"),
            ]),
            Some("zh-CN".to_string())
        );
    }
}
