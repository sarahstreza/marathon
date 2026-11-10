//! Enforces the internal dependency graph from PLAN.md §4.
//!
//! This is a workspace-wide check; it lives here only because the workspace
//! root is a virtual manifest and cannot hold tests. It reads the declared
//! dependencies of every workspace member via `cargo metadata` and checks that
//! each Marathon crate depends (normally, at build time, or for dev) only on
//! the Marathon crates it is allowed to.

// Test code: panicking on malformed `cargo metadata` output is the right failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Every Marathon crate and the internal crates it may depend on.
const ALLOWED: &[(&str, &[&str])] = &[
    ("marathon-api", &[]),
    ("marathon-rules", &[]),
    ("marathon-server", &["marathon-api", "marathon-rules"]),
    ("marathon-client", &["marathon-api"]),
    ("marathon-mpv", &[]),
    ("marathond", &["marathon-server", "marathon-api"]),
    (
        "marathon-desktop",
        &[
            "marathon-client",
            "marathon-mpv",
            "marathon-server",
            "marathon-api",
        ],
    ),
];

fn workspace_deps() -> BTreeMap<String, BTreeSet<String>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("failed to run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();

    let packages = metadata["packages"].as_array().unwrap();
    let members: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect();

    packages
        .iter()
        .map(|p| {
            let internal = p["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["name"].as_str().unwrap().to_owned())
                .filter(|name| members.contains(name))
                .collect();
            (p["name"].as_str().unwrap().to_owned(), internal)
        })
        .collect()
}

#[test]
fn workspace_has_exactly_the_planned_crates() {
    let actual: BTreeSet<_> = workspace_deps().into_keys().collect();
    let expected: BTreeSet<_> = ALLOWED.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(actual, expected);
}

#[test]
fn internal_dependencies_follow_the_plan() {
    let deps = workspace_deps();
    let mut violations = Vec::new();
    for (krate, allowed) in ALLOWED {
        let Some(actual) = deps.get(*krate) else {
            continue; // Reported by workspace_has_exactly_the_planned_crates.
        };
        for dep in actual {
            if !allowed.contains(&dep.as_str()) {
                violations.push(format!("{krate} -> {dep}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "disallowed internal dependencies (see PLAN.md §4): {violations:?}"
    );
}

/// `marathon-rules` must stay pure (PLAN.md §6, M1): no I/O, clocks, or RNG.
/// Its non-dev dependencies are therefore pinned to this reviewed list;
/// adding one should be a deliberate change to this test.
const RULES_ALLOWED_DEPS: &[&str] = &["serde"];

#[test]
fn rules_crate_has_only_pure_dependencies() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output()
        .expect("failed to run cargo metadata");
    assert!(output.status.success());
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let rules = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "marathon-rules")
        .expect("marathon-rules is a workspace member");
    let deps: BTreeSet<&str> = rules["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        // `kind` is null for normal dependencies, "dev" or "build" otherwise.
        .filter(|d| d["kind"].as_str() != Some("dev"))
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    let allowed: BTreeSet<&str> = RULES_ALLOWED_DEPS.iter().copied().collect();
    assert_eq!(
        deps, allowed,
        "marathon-rules dependencies changed; it must stay free of I/O, time, and RNG"
    );
}
