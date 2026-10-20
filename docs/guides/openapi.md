# OpenAPI APIs

mcpgw can serve an HTTP API described by an OpenAPI 3.x spec as MCP tools
itself, with no MCP server involved:

```sh
mcpgw add pets --openapi https://petstore3.swagger.io/api/v3/openapi.json
mcpgw add billing --openapi ./billing.yaml --base-url https://api.example.com/v2 \
  --auth bearerAuth=billing_token
```

```toml
[[server]]
name = "pets"
transport = "openapi"
spec = "/opt/specs/petstore.yaml"           # file or URL, JSON or YAML
base_url = "https://petstore3.swagger.io/api/v3"
headers = { "X-Team" = "tools" }
auth = { api_key = { secret = "petstore_key" } }
```

## From spec to tools

- **One tool per operation.** It is named after the `operationId` (else
  `<method>_<path>`), and described by its summary and description.
- **Input schema:** one property per parameter (path, query, header,
  cookie), plus `body` for the request body.
  - `$ref`s are inlined.
  - OpenAPI 3.0's `nullable` and `example` become JSON Schema.
- **Calls.**
  - Arguments are checked against the schema before any request is sent.
  - The response comes back as JSON or text (capped at 100 KB).
  - 4xx and 5xx responses are tool errors starting `HTTP <status>`.
- **Base URL.** It is `base_url`, else the spec's first server. A relative
  server resolves against the spec's URL. Swagger 2.0 is refused, with a
  hint to convert it.

## Security schemes

Bind a vault secret to each security scheme by its name in the spec:

| Scheme | Sent as |
|---|---|
| `apiKey` in header, query or cookie | that header, query parameter or cookie |
| `http` `basic` | `Authorization: Basic …`; set `username` (`--auth-username scheme=user`), and the secret is the password |
| `http` `bearer` | `Authorization: Bearer <secret>` |
| `oauth2`, `openIdConnect` | `Authorization: Bearer <access token>`; bind an OAuth login: `secret = "oauth/<provider>@{account}"` |

- **Per operation.** Each operation's `security` alternatives are tried in
  order, and the first one whose schemes are all bound is used.
- **Scheme names are checked.** A binding for a scheme the spec doesn't have
  stops the start, with the list of schemes it does have.
- **Missing credentials.** A 401 or 403 when credentials are missing says
  which schemes the operation needs.
- **Redaction.** Values are redacted like every other secret.

`examples/openapi-petstore.toml` is a complete example against the public
Petstore.
