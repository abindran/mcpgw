//! BM25 tool search with tantivy (M3-09).
//!
//! Each tool is a document with four fields: its name (split into words:
//! `browser_take_screenshot` → `browser take screenshot`), its server
//! prefix, its parameter names and descriptions, and its description. All
//! go through the same analyzer (lowercase, English stop words removed,
//! English stemming) and are scored with BM25, with the name weighted
//! most. Query words also match a few common synonyms (folder/directory,
//! make/create, …) at a lower weight. Indexes are cached by the tool set,
//! so a search costs microseconds once built.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use serde_json::Value;
use tantivy::{
    Index, IndexReader, TantivyDocument, Term,
    collector::TopDocs,
    query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery},
    schema::{
        Field, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions, Value as _,
    },
    tokenizer::{
        Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter,
        TextAnalyzer,
    },
};

use mcpgw_runtime::JsonObject;

const ANALYZER: &str = "mcpgw";

/// Field weights: a word in the name says most about what a tool does.
const NAME_BOOST: f32 = 3.0;
const SERVER_BOOST: f32 = 1.5;
const PARAMS_BOOST: f32 = 1.5;
const DESC_BOOST: f32 = 1.0;
/// Weight of a synonym relative to the word itself.
const SYNONYM_BOOST: f32 = 0.5;

/// Common words for the same action or thing in tool vocabularies.
const SYNONYMS: &[(&str, &[&str])] = &[
    ("folder", &["directory"]),
    ("directory", &["folder"]),
    ("rename", &["move"]),
    ("make", &["create"]),
    ("new", &["create"]),
    ("add", &["create"]),
    ("open", &["create", "read"]),
    ("edit", &["update", "modify"]),
    ("modify", &["update", "edit"]),
    ("change", &["update", "edit"]),
    ("delete", &["remove"]),
    ("remove", &["delete"]),
    ("show", &["get", "list"]),
    ("display", &["show", "get"]),
    ("view", &["get", "read"]),
    ("find", &["search"]),
    ("lookup", &["search", "get"]),
    ("look", &["search", "get"]),
    ("fetch", &["get", "retrieve"]),
    ("retrieve", &["get"]),
    ("run", &["execute"]),
    ("execute", &["run"]),
    ("launch", &["start", "run"]),
    ("stop", &["kill"]),
    ("send", &["post"]),
    ("reply", &["comment"]),
    ("save", &["write"]),
    ("download", &["fetch", "get"]),
    ("upload", &["push"]),
    ("who", &["me", "user"]),
    ("switch", &["checkout", "select"]),
    ("stage", &["add"]),
    ("unstage", &["reset"]),
    ("coordinates", &["latitude", "longitude", "geocode"]),
    ("address", &["geocode"]),
    ("altitude", &["elevation"]),
    ("height", &["elevation"]),
    ("nearby", &["local", "places"]),
    ("site", &["website", "url"]),
    ("website", &["site", "url"]),
    ("dark", &["media", "color"]),
    ("time", &["timezone"]),
];

/// Split identifiers into words: snake, kebab, dotted and camel case.
pub fn words(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' || c == '/' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.push(c);
    }
    out
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(StopWordFilter::new(Language::English).expect("English stop words"))
        .filter(Stemmer::new(Language::English))
        .build()
}

fn analyze(text: &str) -> Vec<String> {
    let mut a = analyzer();
    let mut stream = a.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

struct Fields {
    name: Field,
    server: Field,
    params: Field,
    desc: Field,
    pos: Field,
}

/// A searchable index over a set of tools.
pub struct ToolIndex {
    reader: IndexReader,
    f: Fields,
    len: usize,
}

fn params_text(tool: &JsonObject) -> String {
    let mut out = String::new();
    if let Some(p) = tool
        .get("inputSchema")
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    {
        for (k, v) in p {
            out.push_str(&words(k));
            out.push(' ');
            if let Some(d) = v.get("description").and_then(Value::as_str) {
                out.push_str(d);
                out.push(' ');
            }
        }
    }
    out
}

impl ToolIndex {
    /// Index `tools` (public names, `server__tool`).
    pub fn build(tools: &[JsonObject]) -> tantivy::Result<ToolIndex> {
        let mut sb = Schema::builder();
        let text = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(ANALYZER)
                .set_index_option(IndexRecordOption::WithFreqs),
        );
        let f = Fields {
            name: sb.add_text_field("name", text.clone()),
            server: sb.add_text_field("server", text.clone()),
            params: sb.add_text_field("params", text.clone()),
            desc: sb.add_text_field("desc", text),
            pos: sb.add_u64_field("pos", STORED),
        };
        let index = Index::create_in_ram(sb.build());
        index.tokenizers().register(ANALYZER, analyzer());
        let mut w = index.writer_with_num_threads(1, 15_000_000)?;
        for (i, t) in tools.iter().enumerate() {
            let full = t.get("name").and_then(Value::as_str).unwrap_or_default();
            let (server, name) = full.split_once("__").unwrap_or(("", full));
            let mut doc = TantivyDocument::default();
            doc.add_text(f.name, words(name));
            doc.add_text(f.server, words(server));
            doc.add_text(f.params, params_text(t));
            doc.add_text(
                f.desc,
                t.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            doc.add_u64(f.pos, i as u64);
            w.add_document(doc)?;
        }
        w.commit()?;
        let reader = index.reader()?;
        Ok(ToolIndex {
            reader,
            f,
            len: tools.len(),
        })
    }

    /// Positions (into the indexed tools) of the best `limit` matches.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.search_with(query, limit, true)
    }

    /// [`search`](Self::search), optionally without synonym expansion.
    pub fn search_with(&self, query: &str, limit: usize, synonyms: bool) -> Vec<usize> {
        let mut weighted: Vec<(String, f32)> = analyze(&words(query))
            .into_iter()
            .map(|t| (t, 1.0))
            .collect();
        let raws: Vec<String> = if synonyms {
            query
                .split(|c: char| !c.is_alphanumeric())
                .map(str::to_lowercase)
                .collect()
        } else {
            Vec::new()
        };
        for raw in raws {
            if let Some((_, alts)) = SYNONYMS.iter().find(|(w, _)| *w == raw) {
                for a in alts.iter().flat_map(|a| analyze(a)) {
                    if !weighted.iter().any(|(t, _)| *t == a) {
                        weighted.push((a, SYNONYM_BOOST));
                    }
                }
            }
        }
        if weighted.is_empty() {
            return Vec::new();
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (term, w) in &weighted {
            for (field, boost) in [
                (self.f.name, NAME_BOOST),
                (self.f.server, SERVER_BOOST),
                (self.f.params, PARAMS_BOOST),
                (self.f.desc, DESC_BOOST),
            ] {
                let q = TermQuery::new(
                    Term::from_field_text(field, term),
                    IndexRecordOption::WithFreqs,
                );
                clauses.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(Box::new(q), boost * w)),
                ));
            }
        }
        let searcher = self.reader.searcher();
        let top = TopDocs::with_limit(limit.max(1).min(self.len.max(1))).order_by_score();
        let Ok(hits) = searcher.search(&BooleanQuery::new(clauses), &top) else {
            return Vec::new();
        };
        hits.into_iter()
            .filter_map(|(_, addr)| {
                let d: TantivyDocument = searcher.doc(addr).ok()?;
                d.get_first(self.f.pos)
                    .and_then(|v| v.as_u64())
                    .map(|p| p as usize)
            })
            .collect()
    }
}

fn fingerprint(tools: &[JsonObject]) -> u64 {
    let mut h = DefaultHasher::new();
    for t in tools {
        t.get("name").and_then(Value::as_str).hash(&mut h);
        t.get("description").and_then(Value::as_str).hash(&mut h);
        params_text(t).hash(&mut h);
    }
    h.finish()
}

/// Recently built indexes, by tool set (one per endpoint view, typically).
static CACHE: Mutex<Vec<(u64, Arc<ToolIndex>)>> = Mutex::new(Vec::new());
const CACHED: usize = 8;

/// The index for `tools`, built once per distinct tool set.
pub fn index_for(tools: &[JsonObject]) -> Option<Arc<ToolIndex>> {
    let fp = fingerprint(tools);
    if let Some(i) = CACHE
        .lock()
        .ok()?
        .iter()
        .find(|(k, _)| *k == fp)
        .map(|(_, i)| i.clone())
    {
        return Some(i);
    }
    let built = Arc::new(ToolIndex::build(tools).ok()?);
    let mut c = CACHE.lock().ok()?;
    c.retain(|(k, _)| *k != fp);
    if c.len() >= CACHED {
        c.remove(0);
    }
    c.push((fp, built.clone()));
    Some(built)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifier_words() {
        assert_eq!(words("browser_take_screenshot"), "browser take screenshot");
        assert_eq!(words("API-post-search"), "API post search");
        assert_eq!(words("kubectlGet"), "kubectl Get");
        assert_eq!(
            analyze("Listing the open Pull Requests"),
            ["list", "open", "pull", "request"]
        );
    }

    #[test]
    fn ranks_by_bm25_and_caches() {
        let t = |n: &str, d: &str| {
            json!({"name": n, "description": d})
                .as_object()
                .unwrap()
                .clone()
        };
        let tools = vec![
            t("fs__create_directory", "Create a new directory"),
            t("fs__read_file", "Read a file"),
            t("gh__create_issue", "Open an issue"),
        ];
        let i = index_for(&tools).unwrap();
        assert_eq!(i.search("make a folder", 3)[0], 0);
        assert_eq!(i.search("reading files", 3)[0], 1);
        assert!(i.search("the a of", 3).is_empty(), "stop words only");
        assert!(Arc::ptr_eq(&i, &index_for(&tools).unwrap()), "cached");
    }
}
