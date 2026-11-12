//! `config/example.toml` must always parse into the config types.

use std::path::{Path, PathBuf};

use marathon_rules::{GroupBy, Order, Rule};
use marathon_server::config::{Config, SourceConfig, StateMode};

fn example_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/example.toml")
}

#[test]
fn example_config_parses() {
    let config = Config::load(example_path()).unwrap();
    // Including the checks the schema can't express, such as nesting.
    config.validate().unwrap();

    assert_eq!(config.server.bind, "127.0.0.1:7878".parse().unwrap());
    assert_eq!(config.server.token, None);

    assert_eq!(config.sources.len(), 1);
    assert!(matches!(
        &config.sources[0],
        SourceConfig::Filesystem { id, .. } if id == "nas"
    ));

    let ids: Vec<_> = config.channels.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "office",
            "taskmaster",
            "essays",
            "essayists",
            "contrapoints",
            "folding-ideas",
            "doctor-who"
        ]
    );

    let taskmaster = &config.channels[1];
    assert_eq!(taskmaster.number, Some(6));
    assert_eq!(
        taskmaster.rule,
        Rule::Grouped {
            by: GroupBy::Season,
            groups: Order::Shuffled,
            within: Order::Ordered,
        }
    );

    let essayists = &config.channels[3];
    assert_eq!(essayists.children().count(), 2);
    assert_eq!(essayists.state, StateMode::Shared);
    assert!(config.channels[4].hidden && config.channels[4].number.is_none());

    let essays = &config.channels[2];
    assert_eq!(essays.rule, Rule::Shuffled);
    assert_eq!(essays.items.len(), 2);
    assert!(essays.selectors().all(|s| s.recursive && s.source == "nas"));

    let doctor_who = config.channel("doctor-who").unwrap();
    assert_eq!(doctor_who.rule, Rule::Listed);
    let selectors: Vec<_> = doctor_who.selectors().collect();
    assert_eq!(selectors[0].files.len(), 4);
    assert_eq!(selectors[1].exclude.len(), 1);
}

#[test]
fn every_channel_references_a_configured_source() {
    let config = Config::load(example_path()).unwrap();
    for channel in &config.channels {
        for selector in channel.selectors() {
            assert!(
                config.sources.iter().any(|s| s.id() == selector.source),
                "channel {} references unknown source {}",
                channel.id,
                selector.source
            );
        }
    }
}
