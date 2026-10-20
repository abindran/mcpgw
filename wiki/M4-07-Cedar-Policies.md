# M4-07 — Cedar policies

**Story:** US3 · **Estimate:** 4 days · **Depends on:** M2-08
**Done when:** existing allow/deny/approve rules convert to Cedar and give the same decisions.
**Status:** ✅ Done: 11 rule sets × 22 tool names decide the same in glob and Cedar form, and do so end to end through the daemon

## Using it

```sh
mcpgw policy convert --profile frontend -o policies/frontend.cedar   # from the glob lists
mcpgw policy check --profile frontend gh__delete_repo fs__read_file   # load, validate, decide
```

```toml
[[profile]]
name = "frontend"
servers = ["fs", "github"]
policy = { cedar = "policies/frontend.cedar" }   # relative to the config file

[gateway]
policy = { cedar = "/etc/mcpgw/default.cedar" }  # the default endpoint
```

A Cedar file **replaces** the `allow`/`deny`/`approve` lists: a policy with
both is a config error. Profiles created through the API must give an
absolute path.

## The model

| | Cedar |
|---|---|
| principal | `Mcpgw::Endpoint::"<profile>"`, or `"default"` for the default endpoint |
| action | `Mcpgw::Action::"call"` |
| resource | `Mcpgw::Tool::"<prefix>__<tool>"`, in `Mcpgw::Server::"<prefix>"` |
| attributes | `resource.name` (the public name), `resource.server`, `resource.tool` |

```cedar
@id("readers")
permit (principal == Mcpgw::Endpoint::"frontend", action, resource in Mcpgw::Server::"fs")
when { resource.tool like "read_*" };

@id("merges need a human")
@approve
permit (principal, action == Mcpgw::Action::"call", resource)
when { resource.server == "github" && resource.tool like "merge_*" };

@id("no deletes")
forbid (principal, action, resource) when { resource.tool like "delete_*" };
```

- **Decisions:**
  - `forbid` beats `permit`;
  - nothing permitted means deny;
  - a permitting policy with `@approve` makes the call wait for a human
    (M2-09), like the `approve` list.
- **Messages** name the deciding policy by its `@id`, else by its
  position (`policy0`):
  - "Tool fs__delete_file is disabled by policy (profile frontend denies
    `no deletes`)."
  - with nothing permitting: "… (no Cedar policy of profile frontend
    permits it)."
- Hidden from `tools/list` and search, and refused on call, exactly as
  with glob rules (M2-08).
- **Validated on load** against the built-in schema (`mcpgw_cedar::SCHEMA`),
  in strict mode. A typo such as `resource.nmae` or an unknown entity type
  is an error, not a silent deny.
- **Fails closed.** A file that doesn't parse or validate, or a build
  without the `cedar` feature, makes the endpoint deny every tool ("…
  denies `policies/x.cedar (not loaded)`"). The error is logged, reported by
  `mcpgw doctor` (`policy_cedar`, fail), and shown by `mcpgw policy check`.
- **Reloads.** Files are loaded once per endpoint and cached by modification
  time, so an edited file applies from the next request. Decisions are
  cached per tool name.

## Conversion (`mcpgw policy convert`)

| Glob rule | Cedar |
|---|---|
| `deny = ["X"]` | `@id("X") forbid … when { resource.name like "X" };` |
| `approve = ["X"]` | `@id("approve X") @approve("X") permit … when { resource.name like "X" };` |
| `allow = ["X"]` | `@id("allow X") permit … when { resource.name like "X" };` |
| `allow = []` | `@id("allow all") permit (…);` |

- **Same decisions and messages.** A deny names the same pattern, approve
  carries the same pattern, and "nothing allows it" stays a deny with no
  pattern.
- **First match.** The first matching policy in file order names the
  decision, as the first matching pattern does in the lists.
- **`*` carries over** as Cedar's `like` wildcard. `?` (exactly one
  character) has no Cedar equivalent, so conversion refuses it and asks for
  `*`.
- The output is ordinary Cedar, ready to be edited by hand.

## Code

- `mcpgw-cedar` (new crate, cedar-policy 4.14 with default features off):
  - `Cedar::{parse, load}` parses, validates and orders the policies;
  - `PolicyEngine::decide` builds one request with its entities and caches
    the result;
  - `attach(policy, endpoint)` keeps an mtime cache;
  - `convert(policy)` writes Cedar from the lists.
- `mcpgw-core`:
  - `Policy.cedar: Option<PathBuf>` and `Policy.engine: Option<Engine>`
    (not serialized);
  - the `PolicyEngine` trait (core stays free of Cedar);
  - `Policy::decide` delegates to the engine and fails closed without one;
  - `Policy::check` rejects a Cedar file together with the lists.
- `mcpgw-config`: resolves Cedar paths against the config file's directory
  and validates them with `Policy::check`.
- `mcpgw-api`: `PUT /v1/profiles/{name}` validates the same way, and needs an
  absolute path.
- `mcpgw` (bin):
  - `cedar` feature, on by default;
  - `policy::loaded` attaches engines for the default endpoint and for each
    profile endpoint;
  - `mcpgw policy convert|check`;
  - the doctor check `policy_cedar`.
- Router: the denial message for Cedar ("no Cedar policy of … permits it").

## Tests

- `crates/mcpgw-cedar/tests/convert.rs`:
  - **equivalence:** 11 rule sets (every M2-08 shape, approve outside allow,
    overlapping deny and approve, `*` everywhere, quotes and backslashes,
    no match) × 22 tool names (including a control character and non-ASCII), comparing `Decision` values with patterns;
  - `?` is refused;
  - hand-written policies: `in Server`, `@approve` without a value, `@id`
    messages, the principal per endpoint;
  - load errors: attribute typo, unknown entity type, syntax;
  - `attach`, and failing closed.
- `crates/mcpgw/tests/cedar.rs`, end to end:
  - glob decisions from `policy check`, then `policy convert -o`;
  - the config switched to relative Cedar paths gives identical `policy
    check` lines, for a profile and for the default endpoint;
  - `doctor --json` shows `policy_cedar` ok, ok, fail;
  - through the daemon: the listing; refusals with the `@id` and "no Cedar
    policy" messages; a permitted call; an `@approve` call that asks the
    client first; the default endpoint's converted file; and the broken
    profile, which lists nothing and refuses calls.
- `mcpgw-core`: fail closed until loaded, and the engine isn't serialized.
