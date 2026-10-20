# M2-08 — Tool policy

**Story:** US3 · **Estimate:** 3 days · **Depends on:** M2-07
**Done when:** a denied tool returns "disabled by policy"; no silent failure.
**Status:** ✅ Done

## Config

```toml
[gateway]
policy = { deny = ["*__delete_*"] }          # the default endpoint

[[profile]]
name = "frontend"
servers = ["fs", "github"]
policy = { allow = ["fs__read_*", "github__*"], deny = ["github__delete_*"], approve = ["github__merge_*"] }
```

Patterns match **public** tool names (`<prefix>__<tool>`): `*` is any run of
characters, `?` exactly one (`mcpgw_core::glob_match`).

## Rules (`Policy::decide`)

1. a `deny` match → **Deny**
2. else an `approve` match → **Approve** (needs a human per call; enforced
   from M2-09, until then it behaves like allow)
3. else allowed if `allow` is empty or matches; otherwise **Deny** ("only
   allows …")

## Enforcement (`mcpgw-router::Scope`)

The policy is part of each endpoint's scope, along with its servers and
accounts:

- **Hidden:** denied tools are left out of `tools/list`, `search_tools`,
  and hybrid pins.
- **Refused, loudly:** calling one anyway, by name or through `call_tool`,
  never reaches the server. It returns a tool error the model can read:

  > Tool gh__crash is disabled by policy (profile safe denies `gh__crash`).

  > Tool gh__env is disabled by policy (profile narrow only allows gh__add, gh__echo).

  `describe_tool` on a denied tool says the same. Each refusal is logged at
  WARN.

## Tests

- `crates/mcpgw/tests/profiles.rs::policy_denies_with_a_clear_message`:
  - profile `safe` (direct, deny `gh__crash`, `*__spawn`): the denied tools
    aren't listed; calling them returns the exact messages naming the
    pattern; the server is still up afterwards (`gh__crash` never ran)
  - profile `narrow` (meta, allow `gh__add` and `gh__echo`, deny
    `gh__echo`): search finds only `gh__add`; `describe_tool gh__echo` and
    `call_tool gh__env` give the deny and the "only allows" messages
  - the default endpoint is unaffected
- Core: glob cases (prefix, infix, `?`, empty, backtracking) and
  deny > approve > allow precedence.

## Cedar

[M4-07](M4-07-Cedar-Policies.md) adds Cedar as an alternative engine: `policy = { cedar = "file.cedar" }`, and `mcpgw policy convert` writes Cedar from these lists with the same decisions.
