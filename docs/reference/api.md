# HTTP API reference

<!-- Generated from `docs/openapi.json` by `cargo xtask schema`; don't edit. -->

The admin API is served by the daemon on loopback. Every request needs
`Authorization: Bearer <token>` (`mcpgw token`). The OpenAPI document is
`docs/openapi.json`, also served at `GET /api/v1/openapi.json`.

## approvals

### `GET /api/v1/approvals`

Tool calls waiting for approval

Responses: `200` Approvals; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/approvals/{id}/approve`

Approve a waiting call

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/approvals/{id}/deny`

Refuse a waiting call

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

## audit

### `GET /api/v1/audit`

Audit log entries, oldest first

| Parameter | In | Description |
|---|---|---|
| `after` | query |  |
| `limit` | query |  |

Responses: `200` Entries; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/audit/verify`

Verify the audit hash chain

Responses: `200` Result; `400` Error; `404` Error; `409` Error.

## meta

### `GET /api/v1/openapi.json`

This document

Responses: `200` OpenAPI document; `400` Error; `404` Error; `409` Error.

## profiles

### `GET /api/v1/profiles`

List profiles

Responses: `200` Profiles; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/profiles/{name}`

One profile

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` Profile; `400` Error; `404` Error; `409` Error.

### `PUT /api/v1/profiles/{name}`

Create or replace a profile; its endpoint works at once

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Request body: JSON (see `docs/openapi.json`).

Responses: `200` Profile; `400` Error; `404` Error; `409` Error.

### `DELETE /api/v1/profiles/{name}`

Remove a profile

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

## projects

### `GET /api/v1/projects`

Projects (.mcpgw.toml) the daemon knows, with their state

Responses: `200` Projects; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/projects`

Register the project at an absolute path (or read it again); `mcpgw connect` does this

Request body: JSON (see `docs/openapi.json`).

Responses: `200` Project; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/projects/{id}`

One project

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Responses: `200` Project; `400` Error; `404` Error; `409` Error.

### `DELETE /api/v1/projects/{id}`

Untrust and forget a project; its servers stop

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/projects/{id}/trust`

Trust the project file's current content; its servers run. With `sha256`, only if that's still its content

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Request body: JSON (see `docs/openapi.json`).

Responses: `200` Project; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/projects/{id}/untrust`

Stop trusting a project; its servers stop

| Parameter | In | Description |
|---|---|---|
| `id` | path |  |

Responses: `200` Project; `400` Error; `404` Error; `409` Error.

## runtime

### `GET /api/v1/runtime`

Daemon status

Responses: `200` Runtime; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/calls`

Recent tool calls, newest first (names and outcomes only)

| Parameter | In | Description |
|---|---|---|
| `limit` | query |  |

Responses: `200` Calls; `400` Error; `404` Error; `409` Error.

## secrets

### `GET /api/v1/secrets`

Secret metadata (never values)

Responses: `200` Secrets; `400` Error; `404` Error; `409` Error.

### `PUT /api/v1/secrets/{name}`

Store a secret

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Request body: JSON (see `docs/openapi.json`).

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

### `DELETE /api/v1/secrets/{name}`

Delete a secret

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

## servers

### `GET /api/v1/servers`

List servers with their state

Responses: `200` Servers; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/servers`

Add a server (stored, started on first use)

Request body: JSON (see `docs/openapi.json`).

Responses: `201` Added; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/servers/{name}`

One server, with its spec

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` Server; `400` Error; `404` Error; `409` Error.

### `DELETE /api/v1/servers/{name}`

Remove a server

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/servers/{name}/enable`

Enable a server

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` Server; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/servers/{name}/disable`

Disable a server (stops it, hides its tools)

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` Server; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/servers/{name}/restart`

Clear failures and stop; starts fresh on its next call

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `204` Done; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/servers/{name}/pin`

Accept a quarantined server's new tools

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` New pin; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/servers/{name}/logs`

Recent stderr (redacted)

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |
| `lines` | query |  |

Responses: `200` Lines; `400` Error; `404` Error; `409` Error.

### `GET /api/v1/servers/{name}/tools`

A server's pinned tools

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |

Responses: `200` Tools; `400` Error; `404` Error; `409` Error.

## tools

### `GET /api/v1/tools`

Public tools, optionally as a profile or a project sees them

| Parameter | In | Description |
|---|---|---|
| `profile` | query |  |
| `project` | query | A project's id: act as its endpoint (`/mcp/project/<id>`) does, with its servers as well as the global ones |

Responses: `200` Tools; `400` Error; `404` Error; `409` Error.

### `POST /api/v1/tools/{name}/call`

Call a tool as the default endpoint does, or a project's endpoint with `project` (policy, approvals and audit apply); the inspector uses it

| Parameter | In | Description |
|---|---|---|
| `name` | path |  |
| `project` | query | A project's id: act as its endpoint (`/mcp/project/<id>`) does, with its servers as well as the global ones |

Request body: JSON (see `docs/openapi.json`).

Responses: `200` The CallToolResult; `400` Error; `404` Error; `409` Error.
