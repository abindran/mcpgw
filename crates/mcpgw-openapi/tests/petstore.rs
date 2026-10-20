//! M4-05: tools generated from the Swagger Petstore OpenAPI 3.0 spec pass
//! JSON Schema validation, and calls work against a mock Petstore.

use mcpgw_openapi::{RequestExtras, api, parse};
use mcpgw_testkit::petstore::FakePetstore;
use serde_json::{Map, Value, json};

fn spec() -> Value {
    parse(include_str!("fixtures/petstore.json")).unwrap()
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn text(r: &Map<String, Value>) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn petstore_tools_pass_schema_validation() {
    let a = api(&spec(), Some("http://127.0.0.1:1/api/v3"), None).unwrap();
    let tools = a.tools();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(tools.len(), 19, "{names:?}");
    for n in [
        "addPet",
        "getPetById",
        "findPetsByStatus",
        "deletePet",
        "uploadFile",
        "loginUser",
        "getInventory",
    ] {
        assert!(names.contains(&n), "{n} missing: {names:?}");
    }
    for t in &tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{name}"
        );
        assert!(!t["description"].as_str().unwrap().is_empty(), "{name}");
        // every input schema is valid JSON Schema 2020-12 (what MCP uses)
        let mut s = t["inputSchema"].clone();
        assert_eq!(s["type"], "object", "{name}");
        s["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
        if let Err(e) = jsonschema::meta::validate(&s) {
            panic!("{name}: invalid schema: {e}\n{s:#}");
        }
        assert!(!s.to_string().contains("$ref"), "{name}: refs left: {s}");
    }
    // the shapes a model needs
    let by = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap()["inputSchema"].clone();
    let get = by("getPetById");
    assert_eq!(get["required"], json!(["petId"]));
    assert_eq!(get["properties"]["petId"]["type"], "integer");
    let add = by("addPet");
    assert_eq!(add["required"], json!(["body"]));
    assert_eq!(
        add["properties"]["body"]["required"],
        json!(["name", "photoUrls"])
    );
    assert_eq!(
        add["properties"]["body"]["properties"]["status"]["enum"],
        json!(["available", "pending", "sold"])
    );
    // OpenAPI 3.0's `example` becomes JSON Schema `examples`
    assert_eq!(
        add["properties"]["body"]["properties"]["name"]["examples"],
        json!(["doggie"])
    );
    let del = by("deletePet");
    assert!(del["properties"]["api_key"].is_object());
    assert_eq!(by("uploadFile")["properties"]["body"]["type"], "string");
    // a schema-checked call rejects bad arguments before any request
    let v = jsonschema::validator_for(&get).unwrap();
    assert!(v.is_valid(&json!({"petId": 7})));
    assert!(!v.is_valid(&json!({"petId": "seven"})));
    assert!(!v.is_valid(&json!({})));
}

#[test]
fn yaml_and_json_give_the_same_tools() {
    let y = parse(include_str!("fixtures/petstore.yaml")).unwrap();
    let a = api(&y, Some("http://x/api/v3"), None).unwrap();
    let b = api(&spec(), Some("http://x/api/v3"), None).unwrap();
    let names = |a: &mcpgw_openapi::Api| a.ops.iter().map(|o| o.tool.clone()).collect::<Vec<_>>();
    assert_eq!(names(&a), names(&b));
    assert!(
        parse("{\"swagger\": \"2.0\"}")
            .unwrap_err()
            .contains("Swagger 2.0")
    );
    // a relative server URL needs a base unless the spec came from a URL
    assert!(api(&spec(), None, None).unwrap_err().contains("relative"));
    let from_url = api(
        &spec(),
        None,
        Some("https://petstore3.swagger.io/api/v3/openapi.json"),
    )
    .unwrap();
    assert_eq!(from_url.base, "https://petstore3.swagger.io/api/v3");
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_reach_the_api() {
    mcpgw_core::tls::install();
    let fake = FakePetstore::start().await;
    let a = api(&spec(), Some(&fake.base), None).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let none = RequestExtras::default();
    let call = |name: &'static str, v: Value| {
        let (a, http, none) = (&a, &http, &none);
        async move { a.call(http, name, &args(v), none).await }
    };

    // JSON body
    let pet = json!({"name": "doggie", "photoUrls": [], "status": "available", "tags": [{"id": 1, "name": "good"}]});
    let r = call("addPet", json!({"body": pet})).await.unwrap();
    assert!(text(&r).contains("\"name\": \"doggie\""), "{r:?}");
    // path parameter
    let r = call("getPetById", json!({"petId": 1})).await.unwrap();
    assert!(
        text(&r).contains("doggie") && r.get("isError").is_none(),
        "{r:?}"
    );
    // query parameters, including an array
    assert!(
        text(
            &call("findPetsByStatus", json!({"status": "available"}))
                .await
                .unwrap()
        )
        .contains("doggie")
    );
    assert!(
        text(
            &call("findPetsByTags", json!({"tags": ["good", "fluffy"]}))
                .await
                .unwrap()
        )
        .contains("doggie")
    );
    let r = call(
        "updatePetWithForm",
        json!({"petId": 1, "name": "rex", "status": "sold"}),
    )
    .await
    .unwrap();
    assert!(text(&r).contains("rex"), "{r:?}");
    // a header parameter
    let r = call("deletePet", json!({"petId": 1, "api_key": "k-1"}))
        .await
        .unwrap();
    assert_eq!(text(&r), "pet deleted");
    // a raw body
    let r = call("uploadFile", json!({"petId": 2, "body": "PNGDATA"}))
        .await
        .unwrap();
    assert!(text(&r).contains("7 bytes"), "{r:?}");
    assert!(text(&call("getInventory", json!({})).await.unwrap()).contains("available"));
    // HTTP errors are tool errors with their status
    let r = call("getPetById", json!({"petId": 99})).await.unwrap();
    assert_eq!(r["isError"], true);
    assert!(text(&r).starts_with("HTTP 404"), "{r:?}");
    // arguments are checked against the schema before sending
    let n = fake.seen().len();
    let e = call("getPetById", json!({"petId": "seven"}))
        .await
        .unwrap_err();
    assert!(e.contains("don't match getPetById's schema"), "{e}");
    assert_eq!(fake.seen().len(), n, "a bad call reached the API");

    let seen = fake.seen();
    assert!(
        seen.iter()
            .any(|s| s.uri == "/api/v3/pet/findByTags?tags=good&tags=fluffy"),
        "{seen:#?}"
    );
    assert!(seen.iter().any(
        |s| s.method == "DELETE" && s.headers.iter().any(|(k, v)| k == "api_key" && v == "k-1")
    ));
    assert!(seen.iter().any(|s| {
        s.uri.ends_with("/uploadImage")
            && s.headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "application/octet-stream")
    }));
}
