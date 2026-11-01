//! Shared schema for hqc. Source of truth: docs/SPEC.md §3 (Event), §4 (event types),
//! §5 (worker <-> hub frames), §6 (client <-> hub: `/api/ws` frames in [`client_frame`],
//! REST bodies in [`api`]). Keep this crate dependency-light: it is linked into the hub,
//! the worker and (via generated TS types later) the client.

pub mod api;
pub mod client_frame;
pub mod event;
pub mod frame;
pub mod mcp;
pub mod rules;

/// Worker <-> hub protocol version (SPEC §5.1). Bump on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Postgres refuses U+0000 in `text` and `jsonb`; it is replaced by U+FFFD wherever event
/// text is produced or stored (SPEC §5.2).
pub const NUL_REPLACEMENT: char = '\u{FFFD}';

/// `s` with every U+0000 replaced by [`NUL_REPLACEMENT`]; borrowed when there is none.
pub fn replace_nul(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\0') {
        s.replace('\0', "\u{FFFD}").into()
    } else {
        s.into()
    }
}

/// [`replace_nul`] in every string and object key of `v`, recursively.
pub fn replace_nul_json(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if s.contains('\0') {
                *s = s.replace('\0', "\u{FFFD}");
            }
        }
        Value::Array(a) => a.iter_mut().for_each(replace_nul_json),
        Value::Object(m) => {
            if m.keys().any(|k| k.contains('\0')) {
                *m = std::mem::take(m)
                    .into_iter()
                    .map(|(k, v)| (replace_nul(&k).into_owned(), v))
                    .collect();
            }
            m.values_mut().for_each(replace_nul_json);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nul_replaced_everywhere() {
        assert_eq!(replace_nul("a\0b"), "a\u{FFFD}b");
        assert!(matches!(replace_nul("ab"), std::borrow::Cow::Borrowed(_)));
        let mut v = json!({"a\0": ["x\0", {"k": "\0"}], "n": 1, "s": "ok"});
        replace_nul_json(&mut v);
        assert_eq!(
            v,
            json!({"a\u{FFFD}": ["x\u{FFFD}", {"k": "\u{FFFD}"}], "n": 1, "s": "ok"})
        );
    }
}
