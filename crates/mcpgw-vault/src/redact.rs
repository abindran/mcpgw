//! Redaction of bound secret values (M1-12).
//!
//! Anything a server sends back (tool results, error messages, stderr) is
//! scrubbed of the secrets injected into it before it reaches the client or
//! a log. Each value is matched as is and in the encodings a tool might
//! echo it in: standard and URL-safe base64, and percent-encoding.

use std::borrow::Cow;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::Value;

/// Values shorter than this are not redacted: they would match too much
/// unrelated text.
pub const MIN_SECRET_LEN: usize = 4;

/// Replaces known secret values with `[REDACTED:<name>]`.
#[derive(Default, Clone)]
pub struct Redactor {
    /// `(needle, replacement)`, longest needle first.
    needles: Vec<(String, String)>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactor({} patterns)", self.needles.len())
    }
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Redactor {
    /// A redactor for `(name, value)` pairs.
    pub fn new<'a>(secrets: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut needles = Vec::new();
        for (name, value) in secrets {
            if value.len() < MIN_SECRET_LEN {
                continue;
            }
            let replacement = format!("[REDACTED:{name}]");
            let mut forms = vec![
                value.to_string(),
                STANDARD.encode(value),
                STANDARD.encode(value).trim_end_matches('=').to_string(),
                URL_SAFE_NO_PAD.encode(value),
                percent(value),
            ];
            forms.sort();
            forms.dedup();
            for f in forms {
                if f.len() >= MIN_SECRET_LEN {
                    needles.push((f, replacement.clone()));
                }
            }
        }
        needles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        needles.dedup_by(|a, b| a.0 == b.0);
        Self { needles }
    }

    /// Whether there is nothing to redact.
    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Redact a string.
    pub fn redact<'s>(&self, s: &'s str) -> Cow<'s, str> {
        if !self.needles.iter().any(|(n, _)| s.contains(n.as_str())) {
            return Cow::Borrowed(s);
        }
        let mut out = s.to_string();
        for (n, r) in &self.needles {
            if out.contains(n.as_str()) {
                out = out.replace(n.as_str(), r);
            }
        }
        Cow::Owned(out)
    }

    /// Redact every string in a JSON value, including object keys.
    pub fn redact_json(&self, v: &mut Value) {
        if self.is_empty() {
            return;
        }
        match v {
            Value::String(s) => {
                if let Cow::Owned(r) = self.redact(s) {
                    *s = r;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.redact_json(x)),
            Value::Object(o) => {
                let keys: Vec<String> = o.keys().cloned().collect();
                for k in keys {
                    let mut val = o.remove(&k).expect("key exists");
                    self.redact_json(&mut val);
                    o.insert(self.redact(&k).into_owned(), val);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_value_and_encodings() {
        let r = Redactor::new([("gh", "ghp_s3cr3t/value+x"), ("short", "ab")]);
        assert_eq!(
            r.redact("token=ghp_s3cr3t/value+x;"),
            "token=[REDACTED:gh];"
        );
        let b64 = STANDARD.encode("ghp_s3cr3t/value+x");
        assert_eq!(r.redact(&format!("auth {b64}")), "auth [REDACTED:gh]");
        assert_eq!(
            r.redact(&URL_SAFE_NO_PAD.encode("ghp_s3cr3t/value+x")),
            "[REDACTED:gh]"
        );
        assert_eq!(r.redact("q=ghp_s3cr3t%2Fvalue%2Bx"), "q=[REDACTED:gh]");
        assert_eq!(
            r.redact("ab is too short to redact"),
            "ab is too short to redact"
        );
        assert!(matches!(r.redact("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn redacts_nested_json_and_keys() {
        let r = Redactor::new([("k", "SECRETVALUE")]);
        let mut v = json!({"content": [{"type": "text", "text": "env: SECRETVALUE"}],
                           "SECRETVALUE": {"n": 1, "list": ["xSECRETVALUEx"]}});
        r.redact_json(&mut v);
        let s = v.to_string();
        assert!(!s.contains("SECRETVALUE"), "{s}");
        assert_eq!(v["content"][0]["text"], "env: [REDACTED:k]");
        assert_eq!(v["[REDACTED:k]"]["list"][0], "x[REDACTED:k]x");
    }

    #[test]
    fn longer_secret_wins_over_a_contained_one() {
        let r = Redactor::new([("a", "abcd"), ("b", "abcdefgh")]);
        assert_eq!(r.redact("abcdefgh abcd"), "[REDACTED:b] [REDACTED:a]");
    }
}
