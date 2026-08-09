//! A small subset of n8n's expression syntax.
//!
//! n8n marks an expression by prefixing the value with `=`, then evaluates
//! `{{ ... }}` segments as JavaScript. The workflows imported here only use
//! item access, so that subset is handled natively and anything richer is
//! reported rather than guessed at.
//!
//! Supported today:
//!   `={{ $json }}`            the whole item body
//!   `={{ $json.field }}`      dotted access, nested allowed
//!   `={{ $json["field"] }}`   bracket access
//!   `plain text`              returned unchanged
//!
//! Everything else returns [`ExprError::Unsupported`], which callers surface as
//! a node error. Silently returning the raw text would let a workflow appear to
//! run while posting `{{ ... }}` to a live endpoint.

use serde_json::Value;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExprError {
    #[error("unsupported expression '{0}': needs the JavaScript evaluator")]
    Unsupported(String),
}

/// Resolve `raw` against `item`. Values without the `=` prefix are literals.
pub fn resolve(raw: &str, item: &Value) -> Result<Value, ExprError> {
    let Some(body) = raw.strip_prefix('=') else {
        return Ok(Value::String(raw.to_string()));
    };

    let trimmed = body.trim();
    let inner = match trimmed.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
        Some(inner) => inner.trim(),
        // `=something` without braces is a literal in n8n too.
        None => return Ok(Value::String(body.to_string())),
    };

    // A single interpolation spanning the whole value keeps its native type,
    // so `={{ $json }}` yields an object rather than its debug rendering.
    if inner.contains("{{") || inner.contains("}}") {
        return Err(ExprError::Unsupported(raw.to_string()));
    }

    resolve_path(inner, item).ok_or_else(|| ExprError::Unsupported(raw.to_string()))
}

fn resolve_path(expr: &str, item: &Value) -> Option<Value> {
    let rest = expr.strip_prefix("$json")?;
    if rest.trim().is_empty() {
        return Some(item.clone());
    }

    let mut current = item;
    let mut chars = rest.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            '.' => {
                chars.next();
                let key: String = collect_while(&mut chars, |c| {
                    c.is_alphanumeric() || c == '_' || c == '-'
                });
                if key.is_empty() {
                    return None;
                }
                current = current.get(&key)?;
            }
            '[' => {
                chars.next();
                let quote = match chars.peek() {
                    Some(&q @ ('"' | '\'')) => {
                        chars.next();
                        Some(q)
                    }
                    _ => None,
                };
                let key: String = match quote {
                    Some(q) => collect_while(&mut chars, |c| c != q),
                    None => collect_while(&mut chars, |c| c != ']'),
                };
                if quote.is_some() {
                    chars.next(); // closing quote
                }
                if chars.next() != Some(']') {
                    return None;
                }
                current = match key.parse::<usize>() {
                    Ok(idx) if quote.is_none() => current.get(idx)?,
                    _ => current.get(&key)?,
                };
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            _ => return None,
        }
    }

    Some(current.clone())
}

fn collect_while<I, F>(chars: &mut std::iter::Peekable<I>, pred: F) -> String
where
    I: Iterator<Item = char>,
    F: Fn(char) -> bool,
{
    let mut out = String::new();
    while let Some(&c) = chars.peek() {
        if !pred(c) {
            break;
        }
        out.push(c);
        chars.next();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Value {
        json!({
            "name": "example",
            "stats": {"visits": 42, "deep": {"ok": true}},
            "tags": ["a", "b"],
            "odd-key": "dashed"
        })
    }

    #[test]
    fn a_value_without_the_equals_prefix_is_literal() {
        assert_eq!(resolve("hello", &item()).unwrap(), json!("hello"));
        assert_eq!(resolve("{{ $json }}", &item()).unwrap(), json!("{{ $json }}"));
    }

    #[test]
    fn whole_item_keeps_its_type() {
        // The observed respondToWebhook parameter in production.
        assert_eq!(resolve("={{ $json }}", &item()).unwrap(), item());
    }

    #[test]
    fn dotted_and_nested_access() {
        assert_eq!(resolve("={{ $json.name }}", &item()).unwrap(), json!("example"));
        assert_eq!(resolve("={{ $json.stats.visits }}", &item()).unwrap(), json!(42));
        assert_eq!(resolve("={{ $json.stats.deep.ok }}", &item()).unwrap(), json!(true));
    }

    #[test]
    fn bracket_access_for_keys_and_indexes() {
        assert_eq!(resolve("={{ $json[\"name\"] }}", &item()).unwrap(), json!("example"));
        assert_eq!(resolve("={{ $json['odd-key'] }}", &item()).unwrap(), json!("dashed"));
        assert_eq!(resolve("={{ $json.tags[1] }}", &item()).unwrap(), json!("b"));
    }

    #[test]
    fn a_missing_field_is_reported_not_silently_empty() {
        assert!(resolve("={{ $json.nope }}", &item()).is_err());
    }

    #[test]
    fn richer_javascript_is_refused_rather_than_guessed() {
        // These need the JS evaluator. Returning the raw text would let a
        // workflow post literal braces to a live endpoint.
        for expr in [
            "={{ $json.a + $json.b }}",
            "={{ new Date().toISOString() }}",
            "={{ $node['X'].json }}",
        ] {
            assert!(
                matches!(resolve(expr, &item()), Err(ExprError::Unsupported(_))),
                "{expr} should be reported as unsupported"
            );
        }
    }
}
