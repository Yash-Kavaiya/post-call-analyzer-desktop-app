//! Salvaging JSON from model output.
//!
//! Even with explicit instructions, models wrap JSON in markdown fences, prefix
//! it with "Here is the JSON:", or append a trailing explanation. Rejecting
//! those responses outright would throw away good analyses over formatting, so
//! we extract the JSON payload before parsing and only fail if there is none.

/// Pull the first complete JSON object or array out of `raw`.
pub fn extract_json(raw: &str) -> Option<&str> {
    let s = raw.trim();

    // Fenced block: ```json ... ``` or ``` ... ```
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        let after = after.trim_start_matches(['\r', '\n']);
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim();
            if !inner.is_empty() {
                return balanced_span(inner).or(Some(inner));
            }
        }
    }

    balanced_span(s)
}

/// Find the first `{...}` / `[...]` span with balanced delimiters, ignoring
/// braces that appear inside string literals.
fn balanced_span(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let open_idx = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let open = bytes[open_idx];
    let close = if open == b'{' { b'}' } else { b']' };

    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate().skip(open_idx) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == 0x5C {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[open_idx..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_object_passes_through() {
        assert_eq!(extract_json(r#"{"a":1}"#), Some(r#"{"a":1}"#));
    }

    #[test]
    fn strips_markdown_fence() {
        let raw = "```json\n{\"a\":1}\n```";
        assert_eq!(extract_json(raw), Some(r#"{"a":1}"#));
    }

    #[test]
    fn strips_bare_fence() {
        let raw = "```\n{\"a\":1}\n```";
        assert_eq!(extract_json(raw), Some(r#"{"a":1}"#));
    }

    #[test]
    fn ignores_prose_before_and_after() {
        let raw = "Here is the JSON:\n{\"a\":1}\nHope that helps!";
        assert_eq!(extract_json(raw), Some(r#"{"a":1}"#));
    }

    #[test]
    fn handles_nested_objects() {
        let raw = r#"prefix {"a":{"b":[1,2]},"c":3} suffix"#;
        assert_eq!(extract_json(raw), Some(r#"{"a":{"b":[1,2]},"c":3}"#));
    }

    #[test]
    fn braces_inside_strings_do_not_confuse_the_scanner() {
        let raw = r#"{"reason":"agent said {hello} to caller"}"#;
        assert_eq!(extract_json(raw), Some(raw));
    }

    #[test]
    fn escaped_quotes_inside_strings_are_handled() {
        let raw = r#"{"q":"she said \"hi\" then left"}"#;
        assert_eq!(extract_json(raw), Some(raw));
    }

    #[test]
    fn arrays_are_supported() {
        assert_eq!(extract_json("[{\"a\":1}]"), Some("[{\"a\":1}]"));
    }

    #[test]
    fn returns_none_when_there_is_no_json() {
        assert_eq!(extract_json("I cannot help with that."), None);
    }

    #[test]
    fn unbalanced_truncated_output_returns_none() {
        // A response cut off by max_tokens must fail loudly, not half-parse.
        assert_eq!(extract_json(r#"{"a":1, "b":"#), None);
    }
}
