//! The OpenAPI document is valid OpenAPI 3 and covers every route in
//! PLAN.md §8.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use marathon_server::http::ApiDoc;
use serde_json::Value;
use utoipa::OpenApi;

/// Every route of the v1 API (PLAN.md §8), as (method, path).
const ROUTES: &[(&str, &str)] = &[
    ("get", "/api/v1/health"),
    ("get", "/api/v1/version"),
    ("get", "/api/v1/channels"),
    ("get", "/api/v1/channels/{id}/now"),
    ("post", "/api/v1/channels/{id}/progress"),
    ("post", "/api/v1/channels/{id}/advance"),
    ("post", "/api/v1/channels/{id}/previous"),
    ("get", "/api/v1/items/{id}"),
    ("get", "/api/v1/stream/{item_id}"),
    ("get", "/api/v1/subtitles/{item_id}/{n}"),
    ("get", "/api/v1/history"),
    ("post", "/api/v1/library/rescan"),
    ("get", "/api/v1/openapi.json"),
];

fn document() -> String {
    ApiDoc::openapi().to_json().unwrap()
}

#[test]
fn document_is_valid_openapi_3() {
    let json = document();
    let spec = oas3::from_json(&json).expect("document parses as OpenAPI 3.1");
    let version = spec.validate_version().expect("supported OpenAPI version");
    assert_eq!(version.major, 3);
}

#[test]
fn document_covers_every_route_and_nothing_else() {
    let doc: Value = serde_json::from_str(&document()).unwrap();
    let mut actual = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            actual.insert((method.clone(), path.clone()));
        }
    }
    let expected: BTreeSet<_> = ROUTES
        .iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn every_schema_reference_resolves() {
    let doc: Value = serde_json::from_str(&document()).unwrap();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for r in refs {
        let name = r
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("unexpected $ref {r}"));
        assert!(schemas.contains_key(name), "dangling $ref {r}");
    }
}

#[test]
fn document_declares_optional_bearer_auth() {
    let doc: Value = serde_json::from_str(&document()).unwrap();
    assert_eq!(
        doc["components"]["securitySchemes"]["bearer"]["scheme"],
        "bearer"
    );
    assert_eq!(doc["security"], serde_json::json!([{}, { "bearer": [] }]));
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                match (key.as_str(), v) {
                    ("$ref", Value::String(s)) => out.push(s.clone()),
                    _ => collect_refs(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}
