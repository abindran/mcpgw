//! M3-09: tool search benchmark. 227 real tools from 18 popular MCP
//! servers (manifests captured with `mcpgw add`; see the fixture), and 124
//! task-style queries, each with the tool(s) that do it. Done when the
//! right tool is in the top 5 for at least 90% of queries.

use mcpgw_router::meta;
use serde_json::{Map, Value, json};

fn tools() -> Vec<Map<String, Value>> {
    let raw: Vec<Value> = serde_json::from_str(include_str!("fixtures/tools-bench.json")).unwrap();
    raw.into_iter()
        .map(|t| {
            let props: Map<String, Value> = t["params"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, d)| (k.clone(), json!({"type": "string", "description": d})))
                .collect();
            json!({
                "name": format!("{}__{}", t["server"].as_str().unwrap(), t["name"].as_str().unwrap()),
                "description": t["description"],
                "inputSchema": {"type": "object", "properties": props},
            })
            .as_object()
            .unwrap()
            .clone()
        })
        .collect()
}

struct Score {
    top1: usize,
    top5: usize,
    total: usize,
    misses: Vec<String>,
}

fn run(search: impl Fn(&[Map<String, Value>], &str, usize) -> Vec<Map<String, Value>>) -> Score {
    let tools = tools();
    assert!(tools.len() >= 200, "{}", tools.len());
    let queries: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/queries-bench.json")).unwrap();
    let mut s = Score {
        top1: 0,
        top5: 0,
        total: queries.len(),
        misses: Vec::new(),
    };
    for q in &queries {
        let want: Vec<&str> = q["want"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        let hits = search(&tools, q["q"].as_str().unwrap(), 5);
        let names: Vec<&str> = hits.iter().map(|h| h["name"].as_str().unwrap()).collect();
        match names.iter().position(|n| want.contains(n)) {
            Some(0) => {
                s.top1 += 1;
                s.top5 += 1;
            }
            Some(_) => s.top5 += 1,
            None => s.misses.push(format!(
                "{:?} → {names:?} (want {want:?})",
                q["q"].as_str().unwrap()
            )),
        }
    }
    s
}

fn report(label: &str, s: &Score) -> f64 {
    let pct = |n: usize| 100.0 * n as f64 / s.total as f64;
    println!(
        "{label}: {} queries, top-1 {:.1}%, top-5 {:.1}%",
        s.total,
        pct(s.top1),
        pct(s.top5)
    );
    pct(s.top5)
}

fn bm25_plain(tools: &[Map<String, Value>], q: &str, n: usize) -> Vec<Map<String, Value>> {
    let i = mcpgw_router::search::index_for(tools).unwrap();
    i.search_with(q, n, false)
        .into_iter()
        .map(|p| tools[p].clone())
        .collect()
}

#[test]
fn search_top5_accuracy_is_at_least_90_percent() {
    let s = run(meta::search);
    let top5 = report("BM25 + synonyms (search_tools)", &s);
    for m in &s.misses {
        println!("  miss: {m}");
    }
    report("BM25 alone", &run(bm25_plain));
    report("keyword baseline (M1-10)", &run(meta::keyword_search));
    assert!(top5 >= 90.0, "top-5 accuracy {top5:.1}% is under 90%");
}
