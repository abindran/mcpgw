# Profiles and policy

## Profiles

A profile is a separate endpoint with its own servers, presentation,
accounts and policy:

```toml
[[profile]]
name = "frontend"
servers = ["fs", "github", "figma"]      # empty: every enabled server
mode = "hybrid"                          # meta | direct | hybrid
pinned_tools = ["fs__read_file"]         # listed directly in hybrid mode
accounts = { github = "work" }
policy = { deny = ["github__delete_*"], approve = ["github__merge_*"] }
```

- **Reaching it.** Its URL is `http://127.0.0.1:7777/mcp/p/frontend`, or
  `mcpgw connect --profile frontend` over stdio.
- **Managing it.** `mcpgw profiles` lists the profiles. The admin API
  (`PUT /api/v1/profiles/{name}`) creates one that works at once, without a
  restart.
- **Changes reach clients.** Enabling, disabling or adding a server shows
  up at once: `tools/list_changed` for 2025-11-25 sessions, and
  `subscriptions/listen` for 2026-07-28 clients.

## Glob policy

`allow`, `deny` and `approve` hold patterns over public tool names. `*` is
any run of characters, `?` exactly one.

1. A `deny` match refuses the tool.
2. Otherwise an `approve` match needs a human, for each call.
3. Otherwise the tool is allowed if `allow` is empty or matches.

Denied tools are hidden from `tools/list` and search. Calling one anyway
returns an error the model can read: "Tool gh__delete_repo is disabled by
policy (profile frontend denies `github__delete_*`)".

## Approvals

A call to an `approve`-listed tool waits for a human:

- **A 2026-07-28 client** with form elicitation gets an MRTR input
  request.
- **A 2025-11-25 client** is asked on its session.
- **Other clients** wait for `mcpgw approve <id>` / `mcpgw deny <id>`, or the
  web UI. The wait lasts at most `gateway.approval_timeout`.

An approval covers that exact call (tool and arguments) once.
`mcpgw approvals` lists the calls that are waiting.

## Cedar

For rules the lists can't express, a profile (or the default endpoint)
can use a Cedar policy file instead:

```toml
[[profile]]
name = "frontend"
policy = { cedar = "policies/frontend.cedar" }   # relative to the config file
```

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

- **The request.** The principal is `Mcpgw::Endpoint::"<profile>"` (or
  `"default"`). The action is `Mcpgw::Action::"call"`. The resource is
  `Mcpgw::Tool::"<public name>"`, in `Mcpgw::Server::"<prefix>"`, with
  `name`, `server` and `tool` attributes.
- **Decisions.** `forbid` beats `permit`, and nothing permitted means
  refused. A permit with `@approve` needs a human. Messages name the
  deciding policy by its `@id`.
- **Load-time checks.** Files are validated against the built-in schema when
  they load, so a typo is an error, not a silent deny.
- **Fails closed.** A file that doesn't load makes the endpoint refuse
  every tool. `mcpgw doctor` reports it, and so does `mcpgw policy check`.

To move existing rules to Cedar:

```sh
mcpgw policy convert --profile frontend -o policies/frontend.cedar
mcpgw policy check --profile frontend github__delete_repo fs__read_file
```

The converted policies give the same decisions, with the same messages.
Cedar has no single-character wildcard, so a `?` can't convert.
