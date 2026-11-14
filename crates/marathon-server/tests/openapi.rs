//! The OpenAPI document is valid OpenAPI 3 and covers every route in
//! PLAN.md §8.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use marathon_api::{Action, Event};
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
    ("get", "/api/v1/channels/{id}/plan"),
    ("get", "/api/v1/items/{id}"),
    ("get", "/api/v1/stream/{item_id}"),
    ("get", "/api/v1/audio/{item_id}"),
    ("get", "/api/v1/subtitles/{item_id}/{n}"),
    ("get", "/api/v1/history"),
    ("post", "/api/v1/library/rescan"),
    ("get", "/api/v1/clients"),
    ("get", "/api/v1/clients/connect"),
    ("post", "/api/v1/clients/{id}/actions"),
    ("get", "/api/v1/events"),
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

/// The `oneOf` branch of a tagged enum's schema whose tag property
/// (`tag`) is the single value `name`.
fn variant_schema<'a>(doc: &'a Value, schema: &str, tag: &str, name: &str) -> &'a Value {
    let variants = doc["components"]["schemas"][schema]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{schema} has no oneOf"));
    let matching: Vec<_> = variants
        .iter()
        .filter(|v| v["properties"][tag]["enum"] == serde_json::json!([name]))
        .collect();
    assert_eq!(matching.len(), 1, "{schema} variant {name}");
    matching[0]
}

/// Asserts that `value` (an example serialized to JSON) has exactly the
/// properties the schema variant declares, and that all of them are
/// required except those in [`OPTIONAL`].
fn assert_shape(variant: &Value, value: &Value, what: &str) {
    let keys = |v: &Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    let declared = keys(&variant["properties"]);
    let required: BTreeSet<String> = variant["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys(value), declared, "{what}: properties");
    let expected: BTreeSet<String> = declared
        .iter()
        .filter(|k| !OPTIONAL.contains(&k.as_str()))
        .cloned()
        .collect();
    assert_eq!(required, expected, "{what}: required");
}

/// Properties added after the first release, which default when absent so
/// that older clients' events still parse (`resolution_path`, M11).
const OPTIONAL: &[&str] = &["resolution_path"];

#[test]
fn every_action_and_event_appears_with_its_json_shape() {
    let doc: Value = serde_json::from_str(&document()).unwrap();
    for action in Action::examples() {
        let value = serde_json::to_value(&action).unwrap();
        let variant = variant_schema(&doc, "Action", "action", action.name());
        assert_shape(variant, &value, action.name());
    }
    for event in Event::examples() {
        let value = serde_json::to_value(&event).unwrap();
        let variant = variant_schema(&doc, "Event", "event", event.name());
        assert_shape(variant, &value, event.name());
    }
    // Navigate's direction is spelled out.
    let navigate = variant_schema(&doc, "Action", "action", "navigate");
    assert_eq!(
        navigate["properties"]["direction"]["$ref"],
        "#/components/schemas/Direction"
    );
    assert_eq!(
        doc["components"]["schemas"]["Direction"]["enum"],
        serde_json::json!(["up", "down", "left", "right"])
    );
    // A relayed event is an event plus its envelope.
    let relayed = &doc["components"]["schemas"]["RelayedEvent"]["allOf"];
    assert_eq!(relayed[0]["$ref"], "#/components/schemas/Event");
    assert_eq!(
        relayed[1]["required"],
        serde_json::json!(["client_id", "ts_ms"])
    );
}
