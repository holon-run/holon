//! Normalization of caller-supplied response-language hints.
//!
//! The hint only shapes a runtime-owned, one-time bootstrap instruction. It
//! is embedded into a prompt, so it must never carry free-form text: only a
//! strict, allow-listed language tag survives normalization.

/// Normalize a caller-supplied language hint into a safe language tag.
///
/// Accepts BCP-47 style tags (`zh-CN`, `en`) and POSIX locale values
/// (`zh_CN.UTF-8@euro`). Returns `None` when the value cannot be mapped to a
/// language tag, so callers can omit the hint instead of failing.
pub fn normalize_initial_response_language(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // POSIX locale values carry a charset and an optional modifier; only the
    // language and territory parts are meaningful for a response language.
    let base = trimmed
        .split('@')
        .next()
        .unwrap_or(trimmed)
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .trim()
        .replace('_', "-");
    let mut subtags = base.split('-');
    let language = subtags.next()?;
    if !(2..=8).contains(&language.len()) || !is_ascii_alpha(language) {
        return None;
    }
    let language = language.to_ascii_lowercase();
    // `C` and `POSIX` select the C locale, not a spoken language.
    if language == "c" || language == "posix" {
        return None;
    }
    let mut tag = String::new();
    tag.push_str(&language);
    let mut extra = 0usize;
    for subtag in subtags {
        extra += 1;
        if extra > 2 {
            return None;
        }
        let normalized = if subtag.len() == 4 && is_ascii_alpha(subtag) {
            format_script(subtag)
        } else if (subtag.len() == 2 && is_ascii_alpha(subtag))
            || (subtag.len() == 3 && subtag.bytes().all(|byte| byte.is_ascii_digit()))
        {
            subtag.to_ascii_uppercase()
        } else {
            return None;
        };
        tag.push('-');
        tag.push_str(&normalized);
    }
    Some(tag)
}

fn format_script(subtag: &str) -> String {
    let mut chars = subtag.chars();
    match chars.next() {
        Some(first) => {
            let mut script = first.to_ascii_uppercase().to_string();
            script.push_str(&chars.as_str().to_ascii_lowercase());
            script
        }
        None => String::new(),
    }
}

fn is_ascii_alpha(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bcp47_and_posix_language_tags() {
        for (raw, expected) in [
            ("en", "en"),
            ("zh-CN", "zh-CN"),
            ("EN-us", "en-US"),
            ("zh_CN.UTF-8", "zh-CN"),
            ("en_US.UTF-8", "en-US"),
            ("zh_CN.UTF-8@euro", "zh-CN"),
            ("zh-Hans-CN", "zh-Hans-CN"),
            ("zh_hans_cn", "zh-Hans-CN"),
            ("fil", "fil"),
        ] {
            assert_eq!(
                normalize_initial_response_language(raw).as_deref(),
                Some(expected),
                "raw locale {raw:?} should normalize to {expected:?}"
            );
        }
    }

    #[test]
    fn rejects_values_without_a_language_tag() {
        for raw in [
            "",
            "   ",
            "C",
            "POSIX",
            "c",
            "x",
            "zh--CN",
            "-en",
            "en-",
            "en-GB-oed",
            "zh CN",
            "en;rm -rf /",
            "en\nRespond in English",
            "zh-CN\u{202e}",
            "this-is-not-a-language-tag-at-all",
        ] {
            assert_eq!(
                normalize_initial_response_language(raw),
                None,
                "raw locale {raw:?} must not produce a language tag"
            );
        }
    }
}
