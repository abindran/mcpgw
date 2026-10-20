//! Tool description scanner (M3-10): flags tool-poisoning signs in a
//! server's tools before they are approved.
//!
//! Checked in every tool's name, title and description and in its input
//! schema (property names and every `description` in it, however deep):
//!
//! - **hidden Unicode** (high): zero-width and other invisible format
//!   characters, bidirectional overrides, Unicode tag characters (which
//!   can spell out hidden ASCII; the hidden text is decoded in the
//!   report), supplementary variation selectors, terminal escape sequences;
//! - **injection phrases** (high): "ignore previous instructions", hidden
//!   `<IMPORTANT>`/`<system>` blocks, "do not tell the user", "before using
//!   this tool, read …", instructions about other tools ("when this tool is
//!   available …"), sending sensitive files (`~/.ssh`, `mcp.json`, `.env`,
//!   credentials) somewhere, markdown images whose URL carries data (an
//!   exfiltration channel);
//! - **another server's tool by name** (high): one server's description
//!   steering another's (tool shadowing);
//! - **warnings**: descriptions over 4096 characters (parameters over
//!   1024), mentions of sensitive files, long base64 blobs, URLs to raw IP
//!   addresses, HTML comments, email addresses, control characters.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::ToolDef;

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Worth a look.
    Warning,
    /// Looks like poisoning: needs explicit approval.
    High,
}

/// One suspicious thing in one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Tool name (as the server calls it).
    pub tool: String,
    /// Where: `description`, `name`, `param path`, `param path.description`.
    pub location: String,
    /// Severity.
    pub severity: Severity,
    /// Short kind: `hidden_unicode`, `injection`, `shadowing`, `length`, …
    pub kind: String,
    /// What was found, with an excerpt.
    pub message: String,
}

/// Longest description before a warning.
pub const MAX_DESCRIPTION: usize = 4096;
/// Longest parameter description before a warning.
pub const MAX_PARAM_DESCRIPTION: usize = 1024;

struct Pattern {
    kind: &'static str,
    severity: Severity,
    re: Regex,
    what: &'static str,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        let p = |kind, severity, what, re: &str| Pattern {
            kind,
            severity,
            what,
            re: Regex::new(&format!("(?is){re}")).expect("scan pattern"),
        };
        use Severity::*;
        vec![
            p("injection", High, "tells the model to ignore its instructions",
              r"\b(ignore|disregard|forget|override)\b.{0,20}\b(all|any|the|your|previous|prior|above|earlier|other|system)\b.{0,20}\b(instructions?|rules|directions|prompts?|guidelines)\b"),
            p("injection", High, "a hidden instruction block",
              r"<\s*/?\s*(important|instructions?|system|secret|hidden|admin)\s*>|\[\s*(system|instructions?)\s*\]|^#+\s*system\b"),
            p("injection", High, "tells the model to keep something from the user",
              r"\b(do\s*n[o']t|don't|never|without)\s+(tell(ing)?|inform(ing)?|mention(ing)?|reveal(ing)?|show(ing)?|notify(ing)?|ask(ing)?|alert(ing)?)\s+(it\s+to\s+|this\s+to\s+|that\s+to\s+)?(the\s+)?(user|human|anyone)\b|\b(keep|hide)\s+(this|it|that)\s+(secret\s+)?from\s+the\s+user\b|\bthe\s+user\s+(must|should)\s+(not|never)\s+(know|see|be\s+told)\b"),
            p("injection", High, "requires a hidden action before the tool is used",
              r"\bbefore\s+(using|calling|you\s+use|running)\s+this\s+tool\b.{0,80}\b(read|open|cat|send|fetch|include|pass|upload|call)\b"),
            p("injection", High, "addresses the model directly to change its behaviour",
              r"\b(you\s+are\s+now|new\s+instructions?|this\s+is\s+a\s+system\s+message|system\s+prompt|jailbreak)\b"),
            p("shadowing", High, "gives instructions about other tools",
              r"\b(when|whenever|if)\s+this\s+tool\s+is\s+(available|present|installed|loaded)\b|\b(all|any|every|the)\s+other\s+tools?\b.{0,40}\b(must|should|always|instead)\b|\binstead\s+of\s+(using\s+)?(the\s+)?[a-z0-9_\-]+\s+tool\b"),
            p("exfiltration", High, "sends a sensitive file somewhere",
              r"(~/\.ssh|id_rsa|id_ed25519|\.aws/credentials|mcp\.json|\.cursor/|\.env\b|/etc/passwd|private[\s_]key|credentials\.json).{0,160}\b(pass|send|include|upload|post|attach|put|add|append|forward)\b|\b(pass|send|include|upload|post|attach|forward)\b.{0,160}(~/\.ssh|id_rsa|id_ed25519|\.aws/credentials|mcp\.json|\.cursor/|\.env\b|/etc/passwd|private[\s_]key|credentials\.json)"),
            p("sensitive_path", Warning, "mentions a sensitive file",
              r"(~/\.ssh|id_rsa|id_ed25519|\.aws/credentials|mcp\.json|/etc/passwd|credentials\.json)"),
            p("base64", Warning, "contains a long base64 blob",
              r"[A-Za-z0-9+/]{120,}={0,2}"),
            p("raw_ip_url", Warning, "links to a raw IP address",
              r"https?://\d{1,3}(\.\d{1,3}){3}"),
            p("markdown_image", High, "has a markdown image with a query string (an exfiltration channel)",
              r"!\[[^\]]*\]\(\s*https?://[^)\s]*\?[^)]*\)"),
            p("html_comment", Warning, "has an HTML comment", r"<!--"),
            p("email", Warning, "mentions an email address",
              r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"),
        ]
    })
}

fn excerpt(text: &str, start: usize, end: usize) -> String {
    let from = text[..start]
        .char_indices()
        .rev()
        .nth(20)
        .map_or(0, |(i, _)| i);
    let to = text[end..]
        .char_indices()
        .nth(20)
        .map_or(text.len(), |(i, _)| end + i);
    let s: String = text[from..to]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!("…{}…", s.trim())
}

/// Invisible or deceptive characters in `text`, as messages.
fn hidden_chars(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut tags = String::new();
    let mut kinds: Vec<&str> = Vec::new();
    let mut add = |k: &'static str| {
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let u = c as u32;
        match u {
            0xE0000..=0xE007F => {
                add("Unicode tag characters");
                if (0xE0020..=0xE007E).contains(&u) {
                    tags.push(char::from_u32(u - 0xE0000).unwrap_or('?'));
                }
            }
            0x200B..=0x200F | 0x2060..=0x2064 | 0xFEFF | 0x00AD | 0x180E => {
                add("zero-width characters")
            }
            0x202A..=0x202E | 0x2066..=0x2069 => add("bidirectional overrides"),
            0xE0100..=0xE01EF => add("supplementary variation selectors"),
            0x1B if chars.peek() == Some(&'[') || chars.peek() == Some(&']') => {
                add("terminal escape sequences")
            }
            _ => {}
        }
    }
    for k in kinds {
        out.push(if k == "Unicode tag characters" && !tags.is_empty() {
            format!(
                "{k} spelling hidden text: {:?}",
                tags.chars().take(120).collect::<String>()
            )
        } else {
            k.to_string()
        });
    }
    out
}

/// Whether `word` occurs in `text` as a whole identifier.
fn mentions_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

fn control_chars(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{1b}'))
}

fn scan_text(
    tool: &str,
    location: &str,
    text: &str,
    max_len: usize,
    others: &[String],
    out: &mut Vec<Finding>,
) {
    let mut push = |severity, kind: &str, message: String| {
        out.push(Finding {
            tool: tool.to_string(),
            location: location.to_string(),
            severity,
            kind: kind.to_string(),
            message,
        })
    };
    for h in hidden_chars(text) {
        push(
            Severity::High,
            "hidden_unicode",
            format!("hidden characters: {h}"),
        );
    }
    if control_chars(text) {
        push(
            Severity::Warning,
            "control_chars",
            "contains control characters".into(),
        );
    }
    // matched on the visible text, so smuggled spacing can't split phrases
    let visible: String = text
        .chars()
        .filter(|c| !matches!(*c as u32, 0xE0000..=0xE007F | 0x200B..=0x200F | 0x2060..=0x2064 | 0xFEFF | 0x00AD))
        .collect();
    let mut kinds_seen: Vec<&str> = Vec::new();
    for p in patterns() {
        // a sensitive path that's also being sent is reported once, as such
        if p.kind == "sensitive_path" && kinds_seen.contains(&"exfiltration") {
            continue;
        }
        if let Some(m) = p.re.find(&visible) {
            kinds_seen.push(p.kind);
            push(
                p.severity,
                p.kind,
                format!("{}: {}", p.what, excerpt(&visible, m.start(), m.end())),
            );
        }
    }
    let lower = visible.to_lowercase();
    for o in others
        .iter()
        .filter(|o| o.len() >= 6 && o.contains(['_', '-']))
    {
        if mentions_word(&lower, &o.to_lowercase()) {
            push(
                Severity::High,
                "shadowing",
                format!("mentions another server's tool {o:?}"),
            );
        }
    }
    let n = text.chars().count();
    if n > max_len {
        push(
            Severity::Warning,
            "length",
            format!("{n} characters (over {max_len})"),
        );
    }
}

fn scan_schema(
    tool: &str,
    path: &str,
    v: &serde_json::Value,
    others: &[String],
    out: &mut Vec<Finding>,
) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, child) in m {
                if k == "description"
                    && let Some(d) = child.as_str()
                {
                    let loc = if path.is_empty() {
                        "input schema description".to_string()
                    } else {
                        format!("param {path}.description")
                    };
                    scan_text(tool, &loc, d, MAX_PARAM_DESCRIPTION, others, out);
                } else if k == "properties"
                    && let Some(props) = child.as_object()
                {
                    for (name, schema) in props {
                        let p = if path.is_empty() {
                            name.clone()
                        } else {
                            format!("{path}.{name}")
                        };
                        for h in hidden_chars(name) {
                            out.push(Finding {
                                tool: tool.into(),
                                location: format!("param {p}"),
                                severity: Severity::High,
                                kind: "hidden_unicode".into(),
                                message: format!("hidden characters in a parameter name: {h}"),
                            });
                        }
                        // a parameter *name* carrying instructions (schema poisoning)
                        if name.len() > 64 {
                            // read `do_not_tell_the_user` as words, keep `id_rsa` as is
                            let both = format!("{name} {}", name.replace(['_', '-'], " "));
                            scan_text(tool, &format!("param {p}"), &both, 2 * 64 + 1, others, out);
                        }
                        scan_schema(tool, &p, schema, others, out);
                    }
                } else {
                    scan_schema(tool, path, child, others, out);
                }
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                scan_schema(tool, path, x, others, out);
            }
        }
        _ => {}
    }
}

/// Scan one server's tools. `others` are tool names of the *other*
/// servers, for spotting shadowing.
pub fn scan_tools(tools: &[ToolDef], others: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();
    // a name this server has too is its own (two servers can share names)
    let others: Vec<String> = others
        .iter()
        .filter(|o| !tools.iter().any(|t| t.name == **o))
        .cloned()
        .collect();
    for t in tools {
        for h in hidden_chars(&t.name) {
            out.push(Finding {
                tool: t.name.clone(),
                location: "name".into(),
                severity: Severity::High,
                kind: "hidden_unicode".into(),
                message: format!("hidden characters in the name: {h}"),
            });
        }
        if let Some(title) = &t.title {
            scan_text(&t.name, "title", title, 200, &others, &mut out);
        }
        if let Some(d) = &t.description {
            scan_text(
                &t.name,
                "description",
                d,
                MAX_DESCRIPTION,
                &others,
                &mut out,
            );
        }
        scan_schema(&t.name, "", &t.input_schema, &others, &mut out);
    }
    out
}

/// Whether any finding is high severity.
pub fn has_high(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::High)
}
