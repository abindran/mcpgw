# Project servers

A `.mcpgw.toml` at a repository's root lists servers for that project
only: a database for this app, a docs server for this monorepo, a tool
that lives in the repository itself. Clients started in the project get
them along with your global servers. Commit the file and everyone who
clones the repository gets the same setup.

```toml
# .mcpgw.toml
[project]
name = "shop"

[[server]]
name = "db"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
secrets = { DATABASE_URL = "shop_db_url" }

[[server]]
name = "tools"
transport = "stdio"
command = "./scripts/mcp-server"
```

`mcpgw project init` writes a starting file. Editors can check it with
the JSON Schema in `schema/project.schema.json`.

## The file

- **Syntax.** Each `[[server]]` takes the same keys as in the
  [config file](../reference/config.md), and is checked the same way: an
  unknown key or a bad value is reported with its line and column.
- **Paths.** Relative paths resolve against the project's root. That
  covers a stdio `command` containing `/` (a bare `npx` is still looked
  up on `PATH`), `cwd`, a Wasm `path` and its `allow_dirs`, and an OpenAPI
  `spec` file.
- **Working directory.** Stdio servers run in the project's root unless
  they set `cwd`.
- **Secrets.** Bind credentials by vault secret name (`secrets`,
  `${secret:name}` in headers), never by value. Each user stores their
  own with `mcpgw secret set shop_db_url`.

## Trust

A project's servers stay off until you trust its file. Otherwise cloning a
repository could run any command on your machine, or hand any of your
vault secrets to a server of its choosing.

```sh
cd ~/src/shop
mcpgw project trust
```

```
project shop (/Users/me/src/shop)
  .mcpgw.toml sha256 4aab7d2522b4
  not trusted yet
  - db: runs npx -y @modelcontextprotocol/server-postgres; secrets: shop_db_url
  - tools: runs /Users/me/src/shop/scripts/mcp-server
Trust this file and run these servers? [y/N]
```

- **What's recorded.** Trusting records the file's SHA-256 for that path.
- **Edits.** Any change to the file (an edit, a pull, another branch)
  turns its servers off until you trust the new content. The daemon checks
  the file on every request to the project's endpoint, so this happens at
  once, without a restart.
- **The approved content only.** Trusting through the app or the API
  sends the hash you were shown. If the file changed in between, the trust
  is refused.
- **Stopping.** `mcpgw project untrust` turns the servers off again.
- **Audit.** `project.trust`, `project.untrust` and `project.forget` are
  written to the [audit log](observability.md).

While a project's servers are off, its endpoint lists a
`project_not_trusted` tool. Its description, and the answer to calling it,
tell the agent (and so you) what to run. `mcpgw doctor` run inside the
project reports the same, and the [macOS app](macos-app.md) shows a
notification and a **Review and Trust…** button.

## Connecting clients

`mcpgw connect` (the entry `mcpgw client install` writes) looks for a
`.mcpgw.toml` from its working directory upwards. Your home directory
itself never counts. When it finds one, it serves that project's endpoint,
`/mcp/project/<id>`, instead of the default one.

- **`--project <dir>`** names the project explicitly.
- **`--no-project`** ignores it.
- **`--profile`** always serves that profile instead.

Claude Code starts its servers in the project, so the global entry is
enough. For clients that start servers elsewhere, add a project-level
entry:

```sh
mcpgw project init --client claude-code --client cursor --client vscode
```

| Client | File | Entry |
|---|---|---|
| Claude Code | `.mcp.json` | `mcpgw connect` |
| Cursor | `.cursor/mcp.json` | `mcpgw connect --project ${workspaceFolder}` |
| VS Code | `.vscode/mcp.json` | `mcpgw connect --project ${workspaceFolder}` |

These entries run `mcpgw` from `PATH`, so they can be committed.

## What the project's endpoint shows

- **Servers.** The project's servers come first, then your global ones.
  The project's tools use the names written in the file (`db__query`). If
  a global server uses the same prefix, the project's server wins on this
  endpoint and the global one is hidden there.
- **Isolation.** Other endpoints never see a project's servers: not the
  default one, profiles, or other projects. Internally each server is
  named `<name>-p<id>`, which is what you'll see in logs and the audit
  log.
- **The rest is the default endpoint's.** Its mode, policy, approvals and
  accounts all apply. The call log and audit log name the endpoint
  `project <name>`.
- **Tools are pinned.** A project server's tools are read and pinned the
  first time it's turned on, like any other server's, and a later change
  to them quarantines it.
- **After a restart.** Trusted projects are registered again when the
  daemon starts.

## Commands

| Command | Does |
|---|---|
| `mcpgw project init [dir] [--name N] [--client C]…` | writes a `.mcpgw.toml` (never overwrites one) and project-level client entries |
| `mcpgw project trust [dir] [--yes]` | shows what the file runs and trusts that content |
| `mcpgw project untrust [dir]` | turns its servers off |
| `mcpgw project ls` | trusted projects and their state (`trusted`, `changed`, `untrusted`, `missing`, `invalid`) |

The HTTP API has the same operations under `/api/v1/projects` (see the
[API reference](../reference/api.md#projects)).
