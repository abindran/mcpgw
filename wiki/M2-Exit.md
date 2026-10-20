# M2 exit check

**Exit criteria (plan):** OAuth flows work for GitHub, Google and Atlassian;
the approval round-trip works in Claude Code and Cursor; the Windows build
passes the M1 test suite.

## Met

- **Windows build passes the M1 suite.** CI's `windows-latest` job runs the
  whole workspace suite (M1 and M2) on every push. It has been green since
  the daemon handle-inheritance fix (`f6bf766`), through M2-14.

## Covered by automated tests (fakes)

- **OAuth** (`crates/mcpgw-auth`, `crates/mcpgw/tests/oauth.rs`,
  `upstream.rs`): auth code with PKCE S256, refresh before expiry, re-auth
  after revocation, several accounts per provider, and the `github`,
  `google` and `atlassian` presets with their extra parameters. Remote MCP
  login is covered too: RFC 9728 and 8414 discovery, RFC 7591 registration
  or a client ID metadata document, and RFC 8707 resource indicators,
  against `FakeOAuth`.
- **Approvals** (`crates/mcpgw/tests/approvals.rs`): rmcp clients speaking
  2026-07-28 get MRTR, and ones speaking 2025-11-25 get session
  elicitation; clients without elicitation fall back to `mcpgw approve` or
  the web UI. Approvals are single-use and bound to the argument digest
  (`mcpgw-runtime` approvals tests), and denial is covered.

## Checked against real services (20 Oct 2026)

- **Registry install** (M2-14): `mcpgw add io.github.github/github-mcp-server`
  pulled the 2.0.2 image, listed 46 tools, asked, and pinned them.
- **GitHub remote MCP** (`https://api.githubcopilot.com/mcp/`): it answers
  401 with resource metadata pointing at `https://github.com/login/oauth`,
  and `mcpgw add` reports that a login is needed. GitHub's authorization
  server has neither a `registration_endpoint` nor client ID metadata
  documents, so mcpgw's remote login needs an `[[oauth]]` app (a GitHub
  OAuth app's client id) for this server. mcpgw's error message says
  exactly that. A PAT in `github-mcp-server_authorization` also works.

## Still needs a person

- Real logins with accounts: GitHub (an OAuth app, `mcpgw auth connect
  github`), Google (`preset = "google"`), and Atlassian
  (`https://mcp.atlassian.com/v1/sse`, M2-05).
- The approval round-trip from **Claude Code** and **Cursor**: add a server
  with `approve = ["*delete*"]` in a profile, call the tool from the client,
  and answer the prompt (or `mcpgw approve <id>`).
