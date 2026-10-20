# M2-14 — MCP Registry integration

**Story:** US1 · **Estimate:** 2 days · **Depends on:** M1-07
**Done when:** `mcpgw add io.github.github/github-mcp-server` installs and shows the tool list for approval.
**Status:** ✅ Done (verified with the real GitHub server under Docker)

## Using it

```sh
mcpgw registry search github             # name  version  kinds  title
mcpgw registry show io.github.github/github-mcp-server
mcpgw add io.github.github/github-mcp-server          # latest, pinned
mcpgw add io.github.github/github-mcp-server@2.0.2    # a given version
mcpgw add io.example/notes --as notes --env DIR=/data # name it; fill an input
mcpgw add io.github.github/github-mcp-server --remote # the hosted endpoint
mcpgw add io.example/x --package pypi -y              # a package type; don't ask
```

`mcpgw add <name with a />` fetches the entry (`GET
/v0/servers/{name}/versions/{version|latest}`), prints how it will run,
starts it once to read its tools, lists them, and asks `add <server>? [y/N]`
(`-y` skips the question; a declined or unanswered question stores nothing).
An approved server is stored with its tools pinned, exactly like a manual
`mcpgw add`. `mcpgw list` shows where it came from:
`… ghcr.io/github/github-mcp-server:2.0.2 [io.github.github/github-mcp-server@2.0.2]`.

The registry is `gateway.registry_url` (default
`https://registry.modelcontextprotocol.io`).

## Install plans (`mcpgw-registry` crate)

| Entry | Runs as |
|-------|---------|
| npm | `npx -y <pkg>@<version> <package args>` |
| pypi | `uvx <pkg>==<version> <package args>` |
| oci | a [container server](M3-01-Container-Launcher.md): `image = "<image>:<version>"` (a tag already in the identifier is kept), runtime args as `run_args`, package args as `args`; the digest is pinned on add |
| remote (`streamable-http`, `sse`) | an HTTP server with the entry's headers |

- `runtimeHint` replaces the default command.
- **Choice:** npm, then pypi, then oci, using the first one whose runtime
  (`npx`, `uvx`, Docker or Podman) is on `PATH`. If none is, the remote is used, with
  a note saying so. `--package` and `--remote` override this.
- **Versions are always pinned**, from the entry's package version and the
  entry's own version (kept as `registry = {name, version}` on the server).
- **Inputs:** fixed values have their `{variables}` filled in. Non-secret
  inputs take `--env NAME=value`, then their default; a required one
  without a value is an error that names the flag to pass.
- **Secrets** are never written into the config. A secret input becomes the
  vault secret `<server>_<input>` (`github-mcp-server_token`). Env vars are
  injected at spawn; for containers, a runtime arg `-e NAME={secret}`
  becomes an injected `NAME` (passed as `-e NAME`), so the value is never in
  a process list. A bare secret
  `Authorization` header becomes `Bearer ${secret:…}`.
- **Optional secrets** that aren't in the vault are left out, with a hint
  (`mcpgw secret set github-mcp-server_token`, then add again). For a
  **required** secret that is missing, a terminal asks for it (hidden);
  otherwise the command stops and says what to set.
- OCI images are pulled and their digest pinned before the first start,
  so a long download doesn't time out the handshake. Package downloads
  (`npx`, `uvx`) happen at that start, which is allowed 300 s. If the tools
  can't be read (a login is needed, or Docker isn't running) the question
  is still asked, and the tools are pinned on the server's first start.

## GitHub (`io.github.github/github-mcp-server` 2.0.2)

The real entry is in `crates/mcpgw-registry/tests/fixtures/`. It has an OCI
package and a remote.

- With Docker or Podman it becomes a container server for
  `ghcr.io/github/github-mcp-server:2.0.2@sha256:ffced0d7…`, with run flags
  `-p 127.0.0.1:8085:8085 -e GITHUB_OAUTH_CALLBACK_PORT=8085`.
  The optional token is added once `github-mcp-server_token` is set;
  otherwise the server logs in with OAuth on first use.
- Without Docker, or with `--remote`, it uses
  `https://api.githubcopilot.com/mcp/`. That endpoint answers 401 with
  OAuth resource metadata, and `mcpgw add` says to run `mcpgw auth connect
  github-mcp-server`. With `github-mcp-server_authorization` set, the PAT is
  sent as a bearer token.

**Verified (macOS arm64, Docker 27.4 on colima, 20 Oct 2026):** the real
entry, served by a local registry, starting with no image in the cache:

```
$ mcpgw add io.github.github/github-mcp-server
io.github.github/github-mcp-server 2.0.2 via oci ghcr.io/github/github-mcp-server:2.0.2
  runs: ghcr.io/github/github-mcp-server:2.0.2
  container flags: -p 127.0.0.1:8085:8085 -e GITHUB_OAUTH_CALLBACK_PORT=8085
  optional secret github-mcp-server_token is not set; …
pulling ghcr.io/github/github-mcp-server:2.0.2 to pin its digest     (stderr)
pinned ghcr.io/github/github-mcp-server:2.0.2 to sha256:ffced0d76e77428532a2ced185516992d77fd146d69eec2f244e6237d1a97796
github-mcp-server offers 46 tools:
  add_comment_to_pending_review  Add review comment to the requester's latest pending pull request review…
  …
add github-mcp-server? [y/N] y
added server github-mcp-server (46 tools, pin 9c7a0befb486)
```

(Since M3-01 an OCI entry installs as a container server; the first run,
before that, used a plain `docker run` stdio command.) No container is left
behind after the tools are read. Before images were
pulled first, a cold install failed because the download ran inside the
handshake. With the Docker daemon stopped the tools can't be read, and the
question still comes up. The remote plan reached the real endpoint and got
its 401 and login hint.

## Tests

- `crates/mcpgw-registry/tests/plan.rs`: the real GitHub entry gives a
  container server for the pinned image, without the unset optional token.
  Once the token is set it is injected and not in argv. Podman alone also
  runs it. Without Docker or Podman it falls back to the remote (header left out, with a note). `--remote` with the secret set
  gives a `Bearer` header. npm and pypi plans are pinned, fill
  `{variables}`, apply defaults and bind secrets. Missing required inputs,
  unrunnable types (`nuget`) and missing runtimes are explained. Default
  names are valid server names.
- `crates/mcpgw/tests/registry.rs` uses `testkit::registry::FakeRegistry`
  and fake-mcp as the npm runtime. It covers search and show; declining on
  stdin stores nothing; a pinned older version under `--as` is approved
  with `y`, listed with its provenance, and requested at
  `/versions/1.0.0`; `-y` installs the latest; unknown entries and bare
  names give clear errors. A remote entry with a required secret header
  fails with "store it with `mcpgw secret set …`", then installs once the
  secret is set (the fake only answers with that bearer), and the token is
  never printed.
