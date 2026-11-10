//! Bearer-token authentication and the loopback-only rule (PLAN.md §5, §8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{CHANNELS, TestEnv, get, item_id, post};
use marathon_server::config::{Config, ConfigError};
use marathon_server::{App, Server, StartError};

async fn app_with_token(env: &TestEnv) -> App {
    let toml = env.config_toml("token = \"s3cret\"", CHANNELS);
    App::open(Config::from_toml_str(&toml).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn with_a_token_requests_without_it_get_401() {
    let env = TestEnv::new();
    let app = app_with_token(&env).await;
    let stream = format!("/api/v1/stream/{}", item_id(&app, common::VIDEOS[0]).await);

    for uri in [
        "/api/v1/health",
        "/api/v1/channels",
        stream.as_str(),
        "/api/v1/nope",
    ] {
        let reply = get(&app, uri).send().await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(reply.error_code(), "unauthorized");
        assert_eq!(reply.header("www-authenticate"), Some("Bearer"));

        for wrong in [
            "Bearer nope",
            "Bearer s3cre",
            "Bearer s3cret2",
            "s3cret",
            "Basic czNjcmV0",
        ] {
            let reply = get(&app, uri).header("Authorization", wrong).send().await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri} with {wrong}");
        }
    }
    let reply = post(&app, "/api/v1/library/rescan").send().await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn with_a_token_requests_with_it_succeed() {
    let env = TestEnv::new();
    let app = app_with_token(&env).await;
    let stream = format!("/api/v1/stream/{}", item_id(&app, common::VIDEOS[0]).await);

    for uri in ["/api/v1/health", "/api/v1/channels", stream.as_str()] {
        let reply = get(&app, uri)
            .header("Authorization", "Bearer s3cret")
            .send()
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn loopback_without_a_token_needs_no_auth() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert_eq!(app.config().server.bind.ip().to_string(), "127.0.0.1");
    assert_eq!(app.config().server.token, None);
    for uri in ["/api/v1/health", "/api/v1/channels", "/api/v1/history"] {
        assert_eq!(get(&app, uri).send().await.status, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn startup_fails_clearly_on_non_loopback_bind_without_token() {
    let env = TestEnv::new();
    let toml = env
        .config_toml("", CHANNELS)
        .replace("bind = \"127.0.0.1:0\"", "bind = \"0.0.0.0:0\"");
    let config = Config::from_toml_str(&toml).unwrap();

    let Err(err) = Server::bind(config.clone()).await else {
        panic!("server must refuse to start");
    };
    assert!(
        matches!(err, StartError::Config(ConfigError::Invalid(_))),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("0.0.0.0:0"), "{message}");
    assert!(message.contains("server.token must be set"), "{message}");
    // Nothing was created: validation happens before touching the disk.
    assert!(!env.data_dir().exists());

    // With a token, the same bind is accepted.
    let toml = toml.replace("[server]\n", "[server]\ntoken = \"s3cret\"\n");
    let server = Server::bind(Config::from_toml_str(&toml).unwrap())
        .await
        .unwrap();
    assert!(server.local_addr().ip().is_unspecified());
}
