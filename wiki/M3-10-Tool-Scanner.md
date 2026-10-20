# M3-10 — Tool description scanner at install

**Story:** US1 · **Estimate:** 2 days · **Depends on:** M2-14
**Done when:** known poisoning samples are flagged before approval.
**Status:** ✅ Done: 15 of 15 samples flagged; 0 findings on 227 real tools

## What is checked (`mcpgw-core::scan`)

The scanner looks at every tool's name, title and description, and its
input schema: property names, and every `description` in it however deep.
The schema is covered because instructions hidden in parameters ("full
schema poisoning") are a known attack.

| Kind | Severity | Examples |
|------|----------|----------|
| `hidden_unicode` | high | zero-width characters, bidi overrides (Trojan Source), **Unicode tag characters**, whose hidden ASCII is decoded into the report, supplementary variation selectors, terminal escape sequences |
| `injection` | high | "ignore/disregard … previous instructions", `<IMPORTANT>`/`<system>`/`[instructions]` blocks, "do not tell the user", "before using this tool, read …", "you are now …", "system prompt" |
| `shadowing` | high | "when this tool is available …", "all other tools must …", or naming **another server's tool** (identifier-like names such as `merge_pull_request`, as whole words) |
| `exfiltration` | high | a sensitive file (`~/.ssh`, `id_rsa`, `mcp.json`, `.cursor/`, `.env`, `.aws/credentials`, a private key) next to "pass/send/include/upload…" |
| `markdown_image` | high | `![x](https://…?d={result})`: data in an image URL |
| `length` | warning | description over 4096 characters (the longest of the 227 real ones is 2781), or a parameter description over 1024 |
| others | warning | a sensitive path mentioned, long base64, a raw-IP URL, an HTML comment, an email address, control characters |

Phrases are matched on the *visible* text, with invisible characters
removed, so `ig​nore` with a zero-width space is still caught. Long parameter
names are also read with `_` and `-` as spaces. Names a server shares with
another (GitHub's and GitLab's `get_file_contents`) don't count as
shadowing.

## Where it runs

- **`mcpgw add`:** after the tools are read and before anything is stored,
  findings are listed (`HIGH: evil/add (description): a hidden instruction
  block: …`). High-risk findings stop the add, with "not adding evil: its
  tools look poisoned …; if you trust it, add it with `--accept-risk`".
  The audit entry records the counts (`"scan": {"high": 3, "warnings": 0}`).
- **Registry installs (M2-14):** the findings are shown with the tool list.
  `-y` won't approve a risky server. Asked interactively, the question
  becomes `add evil-calc despite 3 high-risk finding(s)? [y/N]`.
- **First start of tools nobody reviewed** (`--no-introspect`, or servers
  in the config file): if they look poisoned they are **quarantined** like a
  drift. The reason says `tool scan: … review with mcpgw scan <server>,
  accept with mcpgw pin <server>`, and pinning is the explicit approval.
- **`mcpgw scan [server]`:** scans the pinned tools, or reads a named
  server's tools live if it has no pin. It prints `server  N tools  H high,
  W warnings` with each finding, and fails if anything is high.

## Tests

- `crates/mcpgw-core/tests/scan.rs`:
  - **Done-when:** 15 known poisoning patterns are each flagged high, with
    the expected kind: the classic `<IMPORTANT>` read-`mcp.json` attack,
    email shadowing, tag-character ASCII smuggling, a bidi-override file
    name, a zero-width-split injection, a poisoned parameter description, a
    poisoned parameter *name*, markdown-image exfiltration, a role takeover,
    a hidden `<system>` block, concealment, line jumping (instructions after
    5000 spaces), ANSI-concealed text, `.env` exfiltration, and naming
    another server's tool.
  - Hidden tag text is decoded into the message.
  - **No false positives:** the 227 real tools of the M3-09 benchmark (18
    servers, each scanned against the others' names) give 0 high findings
    and 0 warnings.
- `crates/mcpgw/tests/scan.rs` (end to end):
  - Adding a fake server whose `add` tool carries the classic payload is
    refused, with the findings shown, while a clean server adds with no
    findings. `--accept-risk` adds it and is audited. `mcpgw scan` fails on
    it but passes the clean one.
  - A registry install of the same is refused with `-y`, shows the
    "despite N high-risk findings" question without it, and adds on `y`.
- `crates/mcpgw-runtime/tests/warm.rs::poisoned_first_capture_is_quarantined`:
  unreviewed poisoned tools found at first start are quarantined, not
  pinned, until approved.
