# M0-07 — Config loader

**Story:** US3 · **Estimate:** 2 days · **Depends on:** M0-06
**Done when:** an invalid config prints file, line and a fix hint; a valid sample loads.
**Status:** ✅ Done

## Behaviour

`Config::load(path)` (and `load_or_default`) reads the TOML file, applies
environment overrides and validates. Any problem is a `ConfigError` that
prints as:

```
/home/me/.config/mcpgw/config.toml:4:1: unknown key `comand` in [[server]]
  hint: did you mean `command`?
```

What is checked, in order:

1. **Unknown keys inside `[[server]]`.** `ServerSpec` flattens its transport,
   so serde's `deny_unknown_fields` can't be used there. The loader reads the
   allowed keys for each transport from the `ServerSpec` JSON Schema, so new
   fields are picked up automatically, and suggests the nearest key by edit
   distance. This runs first, so a typo is not reported as "missing field
   `command`".
2. **Syntax and types** via `toml`, with the error's span turned into
   line:column. Unknown top-level keys get "did you mean"; bad durations get an
   example; missing fields get "add `x = ...`".
3. **Semantic rules** (valid unique names, non-empty command, http(s) URL),
   pointing at the offending value.
4. **Environment overrides** `MCPGW_<SECTION>__<KEY>=<value>`, for example
   `MCPGW_GATEWAY__STORE_PATH=/tmp/x.db`. Values are TOML literals when they
   parse (`true`, `30`, `["a"]`), otherwise strings. Only listed sections can
   be overridden (`gateway` today). An error names the variable responsible,
   for example `environment variable MCPGW_GATEWAY__STORE_PTH: unknown key
   `store_pth`` with a did-you-mean hint. Other `MCPGW_*` variables
   (`MCPGW_CONFIG`, `MCPGW_LOG`) are not overrides.

`examples/config.toml` is a commented sample; a test loads it.

## Decisions

- The plan listed `figment` for loading. It is not used: `toml` spans plus a
  small layer give line-numbered errors, did-you-mean hints and env
  overrides with less code than adapting figment's error model.

## Tests

- `crates/mcpgw-config/src/load.rs`: server-key typo (line, column, hint),
  transport-specific keys, top-level typo, syntax error, bad duration,
  bad/duplicate names, bad URL, env overrides applied, env typos in key and
  section, a bad env value.
- `crates/mcpgw/tests/cli.rs`: the CLI prints `config.toml:4:1: …` and the
  hint.

## Also

CI only cancels superseded runs on pull requests now, so every commit on
`main` gets a full matrix result.
