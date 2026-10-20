//! `mcpgw-openapi` — an OpenAPI 3.x API as MCP tools (M4-05).
//!
//! Every operation becomes a tool: named after its `operationId` (else
//! `<method>_<path>`), described by its summary and description, with an
//! input schema built from its parameters (path, query, header, cookie) and
//! a `body` property for its request body. `$ref`s are inlined (cycles cut),
//! and OpenAPI 3.0's schema dialect is turned into JSON Schema (`nullable`,
//! `example`). A call checks the arguments against that schema, builds the
//! request, and returns the response body (pretty JSON or text); HTTP
//! errors (4xx, 5xx) come back as tool errors with their status.
//!
//! Credentials (M4-06) bind to the spec's security schemes by name and are
//! applied where each scheme says: `apiKey` in a header, query parameter or
//! cookie; HTTP `bearer` and `basic`; `oauth2` and `openIdConnect` as a
//! bearer token. An operation's security requirements (alternatives of
//! scheme sets) pick the first alternative whose schemes all have
//! credentials.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// A JSON object.
pub type JsonObject = Map<String, Value>;

/// Where a parameter goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// `/pet/{petId}`.
    Path,
    /// `?status=available`.
    Query,
    /// A request header.
    Header,
    /// A cookie.
    Cookie,
}

/// One parameter of an operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Name in the request.
    pub name: String,
    /// Name in the tool's arguments (differs on a clash).
    pub arg: String,
    /// Where it goes.
    pub location: Location,
    /// Must be given.
    pub required: bool,
}

/// How a request body is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// `application/json`.
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
    /// Anything else, sent as the `body` string.
    Raw,
}

/// One operation, as a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// Tool name.
    pub tool: String,
    /// HTTP method.
    pub method: String,
    /// Path template, e.g. `/pet/{petId}`.
    pub path: String,
    /// Tool description.
    pub description: String,
    /// Its parameters.
    pub params: Vec<Param>,
    /// Its request body: kind, content type and whether it's required.
    pub body: Option<(BodyKind, String, bool)>,
    /// The tool's input schema.
    pub input_schema: Value,
    /// Security requirements: alternatives, each a set of scheme names
    /// (all needed). Empty means no security.
    pub security: Vec<Vec<String>>,
}

/// A credential for one security scheme.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// An API key, bearer token or OAuth access token.
    Secret(String),
    /// HTTP basic.
    Basic {
        /// User name.
        username: String,
        /// Password.
        password: String,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Secret(_) => f.write_str("Secret(***)"),
            Credential::Basic { username, .. } => write!(f, "Basic({username}, ***)"),
        }
    }
}

/// Credentials by scheme name.
pub type Credentials = BTreeMap<String, Credential>;

/// An API: its operations and where to send them.
#[derive(Debug, Clone)]
pub struct Api {
    /// Base URL requests go to.
    pub base: String,
    /// Operations, in spec order.
    pub ops: Vec<Operation>,
    /// The spec's security schemes (for M4-06).
    pub security_schemes: JsonObject,
}

/// Parse a spec: JSON or YAML text.
pub fn parse(text: &str) -> Result<Value, String> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => serde_saphyr::from_str::<Value>(text)
            .map_err(|e| format!("not a JSON or YAML document: {e}"))?,
    };
    let version = v["openapi"].as_str().unwrap_or_default();
    if !version.starts_with('3') {
        return Err(match v.get("swagger") {
            Some(_) => "Swagger 2.0 specs aren't supported; convert to OpenAPI 3 first".into(),
            None => format!("not an OpenAPI 3.x document (openapi = {version:?})"),
        });
    }
    Ok(v)
}

/// Follow a local `$ref` (`#/components/schemas/Pet`).
fn lookup<'a>(spec: &'a Value, r: &str) -> Option<&'a Value> {
    let path = r.strip_prefix("#/")?;
    let mut cur = spec;
    for part in path.split('/') {
        let part = part.replace("~1", "/").replace("~0", "~");
        cur = cur.get(&part)?;
    }
    Some(cur)
}

/// A schema with its `$ref`s inlined (cycles cut) and OpenAPI 3.0's dialect
/// turned into JSON Schema.
fn inline(spec: &Value, v: &Value, seen: &mut Vec<String>) -> Value {
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                if seen.iter().any(|s| s == r) || seen.len() > 16 {
                    // a cycle (Pet → Category → Pet …): stop here
                    return json!({"type": "object", "description": format!("(recursive {r})")});
                }
                return match lookup(spec, r) {
                    Some(t) => {
                        seen.push(r.to_string());
                        let out = inline(spec, t, seen);
                        seen.pop();
                        out
                    }
                    None => json!({}),
                };
            }
            let mut out = Map::new();
            for (k, x) in o {
                match k.as_str() {
                    // OpenAPI-only keywords JSON Schema doesn't know
                    "xml" | "externalDocs" | "discriminator" | "nullable" => {}
                    "example" => {
                        out.insert("examples".into(), json!([x]));
                    }
                    _ => {
                        out.insert(k.clone(), inline(spec, x, seen));
                    }
                }
            }
            if o.get("nullable") == Some(&Value::Bool(true))
                && let Some(Value::String(t)) = out.get("type").cloned()
            {
                out.insert("type".into(), json!([t, "null"]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| inline(spec, x, seen)).collect()),
        other => other.clone(),
    }
}

fn resolve<'a>(spec: &'a Value, v: &'a Value) -> &'a Value {
    let mut cur = v;
    for _ in 0..8 {
        match cur
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| lookup(spec, r))
        {
            Some(t) => cur = t,
            None => break,
        }
    }
    cur
}

/// A tool name from an operation id, or method and path.
fn tool_name(op: &Value, method: &str, path: &str) -> String {
    let raw = match op["operationId"].as_str() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!("{method}_{}", path.trim_matches('/')),
    };
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_string();
    s.chars().take(56).collect()
}

fn location(s: &str) -> Option<Location> {
    Some(match s {
        "path" => Location::Path,
        "query" => Location::Query,
        "header" => Location::Header,
        "cookie" => Location::Cookie,
        _ => return None,
    })
}

/// Build the tools of `spec`. `base` overrides the spec's server URL (and
/// is needed when that URL is relative and the spec came from a file).
pub fn api(spec: &Value, base: Option<&str>, spec_url: Option<&str>) -> Result<Api, String> {
    let server = spec["servers"][0]["url"].as_str().unwrap_or("");
    let base = match base {
        Some(b) => b.to_string(),
        None if server.starts_with("http://") || server.starts_with("https://") => {
            server.to_string()
        }
        None => match spec_url {
            Some(u) => reqwest::Url::parse(u)
                .and_then(|u| u.join(server))
                .map(|u| u.to_string())
                .map_err(|e| format!("server URL {server:?}: {e}"))?,
            None => {
                return Err(format!(
                    "the spec's server URL {server:?} is relative; set base_url"
                ));
            }
        },
    };
    let base = base.trim_end_matches('/').to_string();
    let global_security = spec["security"].clone();
    let mut ops = Vec::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let Some(paths) = spec["paths"].as_object() else {
        return Err("the spec has no paths".into());
    };
    for (path, item) in paths {
        let item = resolve(spec, item);
        let shared: Vec<Value> = item["parameters"].as_array().cloned().unwrap_or_default();
        for method in ["get", "put", "post", "delete", "patch", "head", "options"] {
            let Some(op) = item.get(method) else { continue };
            let mut tool = tool_name(op, method, path);
            let n = names.entry(tool.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                tool = format!("{tool}_{n}");
            }
            let mut props = Map::new();
            let mut required = Vec::new();
            let mut params: Vec<Param> = Vec::new();
            // operation parameters override path-level ones of the same name and place
            let mut all: Vec<Value> = shared.iter().map(|p| resolve(spec, p).clone()).collect();
            for p in op["parameters"].as_array().into_iter().flatten() {
                let p = resolve(spec, p).clone();
                all.retain(|q| !(q["name"] == p["name"] && q["in"] == p["in"]));
                all.push(p);
            }
            for p in all {
                let (Some(name), Some(loc)) =
                    (p["name"].as_str(), p["in"].as_str().and_then(location))
                else {
                    continue;
                };
                let mut arg = name.to_string();
                if props.contains_key(&arg) {
                    arg = format!("{name}_{}", p["in"].as_str().unwrap_or("param"));
                }
                let mut schema = inline(
                    spec,
                    p.get("schema").unwrap_or(&json!({"type": "string"})),
                    &mut Vec::new(),
                );
                if let (Some(d), Value::Object(o)) = (p["description"].as_str(), &mut schema) {
                    o.entry("description")
                        .or_insert_with(|| Value::String(d.to_string()));
                }
                let req = loc == Location::Path || p["required"] == true;
                if req {
                    required.push(Value::String(arg.clone()));
                }
                props.insert(arg.clone(), schema);
                params.push(Param {
                    name: name.to_string(),
                    arg,
                    location: loc,
                    required: req,
                });
            }
            let mut body = None;
            if let Some(rb) = op.get("requestBody") {
                let rb = resolve(spec, rb);
                let content = rb["content"].as_object().cloned().unwrap_or_default();
                let pick = |pred: &dyn Fn(&str) -> bool| {
                    content
                        .iter()
                        .find(|(ct, _)| pred(ct))
                        .map(|(ct, m)| (ct.clone(), m.clone()))
                };
                let chosen = pick(&|ct: &str| ct.contains("json"))
                    .map(|(ct, m)| (BodyKind::Json, ct, m))
                    .or_else(|| {
                        pick(&|ct: &str| ct == "application/x-www-form-urlencoded")
                            .map(|(ct, m)| (BodyKind::Form, ct, m))
                    })
                    .or_else(|| {
                        content
                            .iter()
                            .next()
                            .map(|(ct, m)| (BodyKind::Raw, ct.clone(), m.clone()))
                    });
                if let Some((kind, ct, media)) = chosen {
                    let mut schema = match kind {
                        BodyKind::Raw => json!({"type": "string"}),
                        _ => inline(
                            spec,
                            media.get("schema").unwrap_or(&json!({"type": "object"})),
                            &mut Vec::new(),
                        ),
                    };
                    if let (Some(d), Value::Object(o)) = (rb["description"].as_str(), &mut schema) {
                        o.entry("description")
                            .or_insert_with(|| Value::String(d.to_string()));
                    }
                    let req = rb["required"] == true;
                    if req {
                        required.push(json!("body"));
                    }
                    props.insert("body".into(), schema);
                    body = Some((kind, ct, req));
                }
            }
            let mut input = json!({"type": "object", "properties": props});
            if !required.is_empty() {
                input["required"] = Value::Array(required);
            }
            let description = [op["summary"].as_str(), op["description"].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(". ");
            let description = if description.is_empty() {
                format!("{} {path}", method.to_uppercase())
            } else {
                description
            };
            let security = op.get("security").unwrap_or(&global_security);
            let security = security
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|req| req.as_object())
                .map(|o| o.keys().cloned().collect())
                .collect();
            ops.push(Operation {
                tool,
                method: method.to_uppercase(),
                path: path.clone(),
                description,
                params,
                body,
                input_schema: input,
                security,
            });
        }
    }
    Ok(Api {
        base,
        ops,
        security_schemes: spec["components"]["securitySchemes"]
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

impl Api {
    /// The tools, as MCP `Tool` JSON objects.
    pub fn tools(&self) -> Vec<JsonObject> {
        self.ops
            .iter()
            .map(|o| {
                let mut t = Map::new();
                t.insert("name".into(), Value::String(o.tool.clone()));
                t.insert("description".into(), Value::String(o.description.clone()));
                t.insert("inputSchema".into(), o.input_schema.clone());
                t
            })
            .collect()
    }

    /// Where each scheme of the alternative this operation can satisfy puts
    /// its credential, or the schemes it would need.
    fn auth_for(&self, op: &Operation, creds: &Credentials) -> Result<RequestExtras, Vec<String>> {
        use base64::Engine;
        if op.security.is_empty() || op.security.iter().any(|alt| alt.is_empty()) {
            return Ok(RequestExtras::default());
        }
        let Some(alt) = op
            .security
            .iter()
            .find(|alt| alt.iter().all(|s| creds.contains_key(s)))
        else {
            return Err(op.security.first().cloned().unwrap_or_default());
        };
        let b64 = |s: String| base64::engine::general_purpose::STANDARD.encode(s);
        let mut x = RequestExtras::default();
        for scheme in alt {
            let def = self
                .security_schemes
                .get(scheme)
                .cloned()
                .unwrap_or(Value::Null);
            let cred = &creds[scheme];
            let token = match cred {
                Credential::Secret(t) => t.clone(),
                Credential::Basic { password, .. } => password.clone(),
            };
            let http_scheme = def["scheme"]
                .as_str()
                .unwrap_or("bearer")
                .to_ascii_lowercase();
            match (def["type"].as_str().unwrap_or_default(), cred) {
                ("apiKey", _) => {
                    let name = def["name"].as_str().unwrap_or(scheme).to_string();
                    match def["in"].as_str().unwrap_or("header") {
                        "query" => x.query.push((name, token)),
                        "cookie" => x.cookies.push(format!("{name}={token}")),
                        _ => x.headers.push((name, token)),
                    }
                }
                ("http", Credential::Basic { username, password }) => x.headers.push((
                    "authorization".into(),
                    format!("Basic {}", b64(format!("{username}:{password}"))),
                )),
                // an http basic scheme bound to one secret holding user:password
                ("http", Credential::Secret(t)) if http_scheme == "basic" => x
                    .headers
                    .push(("authorization".into(), format!("Basic {}", b64(t.clone())))),
                // http bearer, oauth2, openIdConnect
                _ => x
                    .headers
                    .push(("authorization".into(), format!("Bearer {token}"))),
            }
        }
        Ok(x)
    }

    /// Run tool `name`: validate the arguments, send the request, return a
    /// `CallToolResult`. `extra` headers and query parameters are added.
    pub async fn call(
        &self,
        http: &reqwest::Client,
        name: &str,
        args: &JsonObject,
        extra: &RequestExtras,
    ) -> Result<JsonObject, String> {
        self.call_with(http, name, args, extra, &Credentials::new())
            .await
    }

    /// [`call`](Self::call) with credentials for the spec's security schemes.
    pub async fn call_with(
        &self,
        http: &reqwest::Client,
        name: &str,
        args: &JsonObject,
        extra: &RequestExtras,
        creds: &Credentials,
    ) -> Result<JsonObject, String> {
        let op = self
            .ops
            .iter()
            .find(|o| o.tool == name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let (auth, missing) = match self.auth_for(op, creds) {
            Ok(a) => (a, Vec::new()),
            Err(m) => (RequestExtras::default(), m),
        };
        let given = Value::Object(args.clone());
        let validator = jsonschema::validator_for(&op.input_schema)
            .map_err(|e| format!("bad input schema: {e}"))?;
        let problems: Vec<String> = validator
            .iter_errors(&given)
            .take(5)
            .map(|e| e.to_string())
            .collect();
        if !problems.is_empty() {
            return Err(format!(
                "arguments don't match {name}'s schema: {}",
                problems.join("; ")
            ));
        }
        let text = |v: &Value| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let mut path = op.path.clone();
        let mut query: Vec<(String, String)> =
            extra.query.iter().chain(&auth.query).cloned().collect();
        let mut headers: Vec<(String, String)> =
            extra.headers.iter().chain(&auth.headers).cloned().collect();
        let mut cookies: Vec<String> = extra.cookies.iter().chain(&auth.cookies).cloned().collect();
        for p in &op.params {
            let Some(v) = args.get(&p.arg) else { continue };
            match p.location {
                Location::Path => {
                    let enc: String = text(v)
                        .bytes()
                        .map(|b| match b {
                            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                                (b as char).to_string()
                            }
                            _ => format!("%{b:02X}"),
                        })
                        .collect();
                    path = path.replace(&format!("{{{}}}", p.name), &enc);
                }
                Location::Query => match v {
                    Value::Array(items) => {
                        query.extend(items.iter().map(|i| (p.name.clone(), text(i))))
                    }
                    _ => query.push((p.name.clone(), text(v))),
                },
                Location::Header => headers.push((p.name.clone(), text(v))),
                Location::Cookie => cookies.push(format!("{}={}", p.name, text(v))),
            }
        }
        let url = format!("{}{path}", self.base);
        let mut full = reqwest::Url::parse(&url).map_err(|e| format!("{url}: {e}"))?;
        if !query.is_empty() {
            full.query_pairs_mut().extend_pairs(query.iter());
        }
        let method =
            reqwest::Method::from_bytes(op.method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = http.request(method, full);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if !cookies.is_empty() {
            req = req.header("cookie", cookies.join("; "));
        }
        if let (Some((kind, ct, _)), Some(b)) = (&op.body, args.get("body")) {
            req = match kind {
                BodyKind::Json => req.json(b),
                BodyKind::Form => {
                    let pairs: Vec<(String, String)> = b
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(k, v)| (k.clone(), text(v)))
                        .collect();
                    req.form(&pairs)
                }
                BodyKind::Raw => req.header("content-type", ct).body(text(b)),
            };
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("{} {url}: {e}", op.method))?;
        let status = resp.status();
        let is_json = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("json"));
        let body = resp.text().await.unwrap_or_default();
        let shown = if is_json {
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or(body)
        } else {
            body
        };
        const MAX: usize = 100_000;
        let mut shown: String = shown.chars().take(MAX).collect();
        if status.is_client_error() || status.is_server_error() {
            shown = format!("HTTP {status}\n{shown}");
            if matches!(status.as_u16(), 401 | 403) && !missing.is_empty() {
                shown.push_str(&format!(
                    "\n(this operation needs credentials for {}; bind them in the server's `auth`)",
                    missing.join(" + ")
                ));
            }
        }
        let mut out = Map::new();
        out.insert("content".into(), json!([{"type": "text", "text": shown}]));
        if status.is_client_error() || status.is_server_error() {
            out.insert("isError".into(), Value::Bool(true));
        }
        Ok(out)
    }
}

/// Added to a request: fixed headers, or credentials (M4-06).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestExtras {
    /// Headers.
    pub headers: Vec<(String, String)>,
    /// Query parameters.
    pub query: Vec<(String, String)>,
    /// Cookies, `name=value`.
    pub cookies: Vec<String>,
}
