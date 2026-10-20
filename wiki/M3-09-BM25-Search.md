# M3-09 — BM25 tool search with tantivy

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M1-10
**Done when:** top-5 accuracy is at least 90% on a 200-tool benchmark set.
**Status:** ✅ Done: top-5 **96.8%**, top-1 81.5% on 227 real tools

## What changed

`search_tools` (meta mode, M1-10) now ranks with BM25, using tantivy 0.27
with an in-RAM index (`mcpgw-router::search`):

- **One document per tool, four fields:** the tool name, split into words
  (`browser_take_screenshot` → browser take screenshot, `API-post-search`,
  `kubectlGet`), with weight 3. The server prefix, weight 1.5. Parameter
  names and descriptions, weight 1.5. The description, weight 1.
- **Analyzer**, the same for documents and queries: simple tokenizer, then
  lowercase, then English stop words removed, then English stemming
  (`pods` → pod, `listing` → list).
- **Synonyms:** about 40 common pairs from tool vocabularies (folder ↔
  directory, make/new → create, rename → move, edit → update, find →
  search, who → me/user, altitude → elevation, …) also match, at half
  weight.
- **Caching:** indexes are cached per distinct tool set (by a fingerprint of
  names, descriptions and parameters; 8 kept), so each endpoint view is
  built once (milliseconds for a few hundred tools) and later searches cost
  microseconds. A tool change makes a new fingerprint, so the next search
  rebuilds.
- An empty query still lists tools by name. The old keyword scorer remains
  as `meta::keyword_search`, the fallback if an index can't be built and the
  benchmark baseline.

## Benchmark

`crates/mcpgw-router/tests/search_bench.rs`:

- **Tools:** 227 real ones from 18 popular servers, with manifests captured
  by `mcpgw add` into `tests/fixtures/tools-bench.json`: GitHub (46),
  Firecrawl (28), Playwright (25), Notion (24), Kubernetes (23),
  filesystem (14), everything (13), git (12), GitLab (9), memory (9),
  Slack (8), Google Maps (7), Brave, Context7, time (2 each), fetch,
  Postgres and sequential-thinking (1 each). They were listed with
  placeholder tokens; nothing was called.
- **Queries:** 124 task-style queries in the words a model would use ("make
  a folder", "who am I on github", "pod logs", "altitude of a location"),
  each with the tool or tools that do it (`queries-bench.json`). Several
  answers are accepted where two tools genuinely fit.

| Search | Top-1 | Top-5 |
|--------|-------|-------|
| **BM25 + synonyms** (`search_tools`) | **81.5%** | **96.8%** |
| BM25 alone | 75.8% | 91.1% |
| M1-10 keyword search | 58.9% | 80.6% |

BM25 alone already meets the bar; the synonyms add almost six points.
The remaining misses are close calls in vocabulary: "stage files for
commit" and "commit staged changes" rank GitHub's commit tools above
`git_add` and `git_commit`, "list pods" ranks `exec_in_pod` above
`kubectl_get`, and "list all urls on a site" misses `firecrawl_map`.
The test prints every miss.

Other tests: identifier splitting and analysis, ranking, stop-word-only
queries and cache reuse (unit tests). The meta-mode and prewarm e2e tests
(M1-10, M3-07) run on the new ranking.
