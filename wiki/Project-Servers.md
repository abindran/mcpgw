# Project servers (after M4)

**Not a plan item.** Asked for after v1.0 was prepared: MCP servers that
belong to one project. Decisions made with the user:

- a project's clients see the **global servers plus the project's**, and
  the project's win a name clash;
- an untrusted or changed file **blocks its servers until it's trusted**.

## What was built

- **`.mcpgw.toml`** at a repository's root (`mcpgw_config::project`):
  - `[project] name` and `[[server]]` entries, with the config file's
    syntax, key checks and positioned errors;
  - relative paths resolve against the root, and stdio servers run there;
  - internal server names are `<name>-p<id>`, where the id is 8 hex digits
    of the SHA-256 of the root's path;
  - `schema/project.schema.json`.
- **Manager** (`mcpgw-runtime`):
  - servers can have an owner project; `names()` and `server_for_prefix()`
    leave owned servers out, so no other endpoint can reach them;
  - `names_in(project)` lists the project's servers, then the global ones
    whose prefix none of its own uses;
  - `server_for_prefix_in` resolves a prefix as the project sees it;
  - a prefix only has to be unique among servers with the same owner.
- **Router:** `Scope.project`, and a `Notice`. While it returns a message,
  the `project_not_trusted` tool is listed and answers with it. With a
  notice, a meta-mode endpoint also tells clients when the tool list
  changes.
- **Daemon** (`mcpgw_api::projects`):
  - the registry, with the endpoint `/mcp/project/<id>`;
  - trust records are kept in the store's key-value table
    (`projects.trusted`: root → SHA-256, name, time);
  - the file is `stat`-ed on every request and read again when it
    changed;
  - turning servers on reads and pins their tools, like config servers at
    startup;
  - trusted projects are registered when the daemon starts;
  - `project.trust`, `project.untrust` and `project.forget` are audited.
- **API:** `GET/POST /api/v1/projects`, `GET/DELETE
  /api/v1/projects/{id}`, and `POST /api/v1/projects/{id}/trust`
  (optionally `sha256`, so only the content shown is trusted) and
  `/untrust`.
- **CLI:**
  - `mcpgw project init|trust|untrust|ls`;
  - `mcpgw connect` finds the project (or `--project` / `--no-project`)
    and logs its state;
  - a `mcpgw doctor` check for the working directory's project;
  - `Client::project_config` and `project_entry` for committed client
    files (`${workspaceFolder}` for Cursor and VS Code).
- **macOS app:**
  - a Projects screen: what each server runs and which secrets it gets,
    and a trust sheet that sends the hash shown;
  - Untrust, Forget, and Add Project (which offers `mcpgw project init`);
  - a sidebar badge, menu bar and overview entries, and a notification
    once per file content.
- **Docs:** [docs/guides/projects.md](../docs/guides/projects.md).

## Tests

- **`crates/mcpgw/tests/projects.rs`**
  - **Untrusted project:** only the notice and the global tools.
  - **After CLI trust:**
    - the project's servers appear;
    - its `echo` beats the global `echo` there;
    - the default endpoint and `mcpgw list` don't show them.
  - **Isolation:** a second project sees its own `echo`.
  - **Edits:** an edit turns the servers off and brings the notice back;
    `ls` and doctor report `changed`; re-trusting turns them on again.
  - **Restart:** the endpoint works after a restart without
    re-registering.
  - **Untrust:** turns them off.
  - **`connect`:** finds the project from a subdirectory; `--no-project`
    and `--project` work.
  - **`init`:** writes the file and client entries and keeps existing
    servers; a second `init` keeps the file; trusting without a terminal
    needs `--yes`.
- **`api.rs`:** every project endpoint, including a trust refused for a
  stale hash (409) and a relative path (400).
- **`project.rs` unit tests:** loading, paths and names; the nearest root;
  error positions and the name limit.
- **The macOS app's end-to-end test:** register, a stale-hash trust,
  trust, the tools pinned, untrust, forget.
- **Checked by hand:**
  - a client started with `mcpgw connect` in an untrusted project got
    `project_not_trusted` plus the global tools, and its log line said
    what to run;
  - the app's Projects screen showed both projects, the servers' commands
    and secrets, and Review and Trust.
