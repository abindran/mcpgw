# M4-05 — OpenAPI-to-MCP adapter

**Story:** US1 · **Estimate:** 5 days · **Depends on:** M2-04
**Done when:** tools generated from a petstore spec pass schema validation and calls.
**Status:** ✅ Done: 19 Petstore tools, all valid JSON Schema 2020-12; calls work through the daemon

## Using it

```sh
mcpgw add pets --openapi https://petstore3.swagger.io/api/v3/openapi.json      # spec URL: relative servers resolve against it
mcpgw add billing --openapi ./billing.yaml --base-url https://api.example.com/v2 \
  --header "Authorization=Bearer \${secret:billing_token}"
```

```toml
[[server]]
name = "pets"
transport = "openapi"
spec = "/opt/specs/petstore.yaml"          # file or URL; JSON or YAML
base_url = "https://petstore3.swagger.io/api/v3"
headers = { "X-Team" = "tools" }
```

No MCP server is involved: mcpgw serves the operations itself, in-process
(over `connect_local`). They get the gateway's pins and drift checks,
policy, approvals, audit and status like any server.

## From spec to tools (`mcpgw-openapi`)

- **One tool per operation.** It is named after the `operationId` (else
  `<method>_<path>`), cleaned to `[A-Za-z0-9_-]`, at most 56 characters,
  and deduplicated. Its description is the summary plus the description.
- **Input schema:** an object with one property per parameter (path,
  query, header, cookie). Path parameters are always required. A `body`
  property holds the request body: JSON as its schema, a form as an object,
  anything else (binary, text) as a string.
  - Path-level parameters are merged in, and operation parameters override
    them.
  - Name clashes across locations get a suffix (`id_header`).
  - Parameter descriptions carry over.
- **Schemas:**
  - `$ref`s (local `#/components/...`) are **inlined**; cycles are cut with
    a described placeholder, so no client needs to resolve references.
  - OpenAPI 3.0's dialect becomes JSON Schema: `nullable: true` turns into
    a `["T","null"]` type, `example` into `examples`, and `xml`,
    `externalDocs` and `discriminator` are dropped.
  - OpenAPI 3.1 schemas pass through.
- **Calls:**
  - arguments are **checked against the input schema first** (jsonschema);
    a mismatch is refused before any request;
  - path values are percent-encoded, array query values repeated (`form`,
    exploded), headers and cookies set, the body sent by kind;
  - the response comes back as pretty JSON or text (capped at 100 KB);
  - **4xx and 5xx are tool errors** (`isError`), starting `HTTP <status>`.
- **Base URL:** `base_url`, else the spec's first server. A relative server
  (`/api/v3`) resolves against the spec's URL, or needs `base_url` when the
  spec is a file. Swagger 2.0 is refused, with a hint to convert it.
- Security schemes are mapped to vault credentials:
  [M4-06](M4-06-OpenAPI-Auth.md).

## Tests

Fixtures: the Swagger Petstore 3.0 spec as JSON and YAML (Apache-2.0).
`testkit::petstore::FakePetstore` is an in-memory Petstore that records
requests.

- `crates/mcpgw-openapi/tests/petstore.rs`:
  - **Done-when (schemas):** all 19 operations become tools with valid
    names and descriptions. Every input schema validates against the
    **JSON Schema 2020-12 meta-schema** and contains no `$ref`. They have
    the right shapes: `petId` an integer and required; `body` required,
    with `name` and `photoUrls` required inside it; the `status` enum; the
    `examples` conversion; `api_key` as a header; the upload as a string.
  - **Done-when (calls)** against the mock: `addPet` (a JSON body),
    `getPetById` (a path parameter), `findPetsByStatus` and
    `findPetsByTags` (repeated `tags=good&tags=fluffy`), `updatePetWithForm`,
    `deletePet` (the `api_key` header), `uploadFile` (a raw octet-stream
    body) and `getInventory`. A 404 is a tool error, and a schema mismatch
    never reaches the API.
  - YAML and JSON give the same tools. Swagger 2.0 is refused. A relative
    server needs a base URL, or resolves against the spec's URL.
- `crates/mcpgw/tests/openapi.rs`, end to end through the daemon: one
  server from the YAML file with `--base-url`, one from the API's own spec
  URL. The tools are listed, `addPet` then `getPetById` works across them, a
  404 is a tool error, and bad arguments are refused with a schema message.

Licenses: `jsonschema` brings in `borrow-or-share` (MIT-0, MIT without
attribution), now on `deny.toml`'s allow list.
