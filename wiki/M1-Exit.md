# M1 exit check

**Exit criteria (plan):** ten popular servers verified on macOS and Linux; the
US2, US3 and US4 MVP acceptance criteria pass; one-week dogfood by both
engineers.

## Verified (macOS arm64, 20 Oct 2026)

Each was installed with `mcpgw add <name> --command npx -- -y <package>`
(introspected and pinned), checked with `mcpgw list --check`, then called once
through `mcpgw serve --stdio`. Every server started lazily on its first call.

| Server | Package | Tools | Protocol | Call |
|--------|---------|-------|----------|------|
| everything | `@modelcontextprotocol/server-everything` | 13 | 2025-11-25 | `get-sum 20 22` → "The sum of 20 and 22 is 42." |
| filesystem | `@modelcontextprotocol/server-filesystem /tmp` | 14 | 2025-11-25 | `list_allowed_directories` → `/tmp`, `/private/tmp` |
| memory | `@modelcontextprotocol/server-memory` | 9 | 2025-11-25 | `create_entities` → entity echoed back |
| sequential-thinking | `@modelcontextprotocol/server-sequential-thinking` | 1 | 2025-11-25 | `sequentialthinking` → thought recorded |

Linux and Windows run the whole automated suite in CI (fake servers covering
both protocol versions, crash, drift, secrets and process-tree cleanup).

## Acceptance criteria covered by automated tests

- **US2 secrets:** the vault holds no plaintext; the passphrase path works
  headless; the env-echo canary is redacted end to end; `client import`
  moves credentials into the vault.
- **US3 one config:** `client import` leaves one `mcpgw connect` entry; meta
  mode finds servers enabled at runtime within 1 s; `mcpgw add` and
  `enable`/`disable` apply live.
- **US4 on demand:** ten unused servers run no processes; idle servers stop;
  LRU eviction under `max_running`; concurrent first calls spawn once;
  process trees are killed, including grandchildren.

## Still needs a person

- Servers that need accounts or credentials: GitHub, Postgres, Slack,
  Playwright (browser download), and others, to reach ten.
- The same smoke run on a Linux desktop.
- The recorded M0/M1 demos with Claude Code and Cursor.
- The one-week dogfood.
