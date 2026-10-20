# M4-11 — v1.0 docs site, upgrade guide and migration notes

**Story:** X · **Estimate:** 3 days · **Depends on:** all
**Done when:** docs cover every CLI command, config key and API endpoint.
**Status:** ✅ Done: the three references are generated from the code, and a test fails when any of them is stale or misses a command, key or endpoint.

## The site (`docs/`, an mdBook)

| | |
|---|---|
| [docs/README.md](../docs/README.md) | what mcpgw is, where to start |
| [getting-started.md](../docs/getting-started.md) | install, five minutes to a working setup |
| [upgrading.md](../docs/upgrading.md) | the upgrade guide: what carries over, behaviour changes, new options, migration notes (glob → Cedar, `headers` → `auth`, timeouts), downgrading |
| `guides/` | servers (all transports, pins, lifecycle, containers, limits), secrets and OAuth, profiles and policy (globs, approvals, Cedar), Wasm tools, OpenAPI APIs, long calls and tasks, the dev loop, observability and troubleshooting |
| `reference/cli.md` | **every command, option and argument**, from clap |
| `reference/config.md` | **every config key**, from `schema/config.schema.json`, including each transport's keys |
| `reference/api.md` | **every HTTP endpoint**, from `docs/openapi.json` |
| `adr/` | the architecture decisions (M0-09) |

`docs/SUMMARY.md` and `docs/book.toml` make it an mdBook: `mdbook build
docs` writes `target/book`. CI builds it on every push (the `docs` job),
and a missing page fails the build.

The repository also has [CHANGELOG.md](../CHANGELOG.md), with the v1.0.0
entry (Added, Changed, Fixed) and the earlier milestones, and
`examples/openapi-petstore.toml`, a runnable OpenAPI example.

## Generated, so they can't drift

- **CLI.** A hidden command, `mcpgw __reference cli`, walks clap's command
  tree (`crates/mcpgw/src/reference.rs`).
  - Each command gets its about text, its aliases and its usage.
  - Each argument gets its flags, value names and help, plus possible
    values, defaults, "repeatable" and "required".
  - Global options are listed once.
- **Config.** `tests/reference.rs` walks the JSON Schema.
  - Each object gets a table of key, type and description (with its
    default).
  - Nested types are reached through options, lists and maps.
  - Each transport variant (`transport = "stdio"`, …) gets its own table.
- **API.** The same test turns the OpenAPI document into one section per
  operation, grouped by tag, with parameters, body and responses.
- **Regenerating.** `cargo xtask schema` now regenerates the references too,
  after the schemas they come from.

## Tests (`crates/mcpgw/tests/reference.rs`)

- `cli_reference_covers_every_command`: every command `mcpgw --help` lists
  has a section, and `docs/reference/cli.md` is current.
- `config_reference_covers_every_key`: every property of every object in the
  schema appears in a table (it caught four missed nested keys during
  development), and `config.md` is current.
- `api_reference_covers_every_endpoint`: one section per operation in the
  OpenAPI document, and `api.md` is current.
- `docs_links_resolve`: every relative link in `docs/` and the README
  points at a file, and every page is in `SUMMARY.md`.
