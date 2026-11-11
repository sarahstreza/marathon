//! Error types: [`Error`] for internal failures, [`ApiError`] for what an
//! HTTP request gets back.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use marathon_api::{ErrorBody, ErrorCode};

/// An internal failure: something the client could not have avoided.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error")]
    Db(#[from] sqlx::Error),
    #[error("invalid JSON")]
    Json(#[from] serde_json::Error),
    #[error("background task failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("I/O error")]
    Io(#[from] std::io::Error),
}

/// An error response from the API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("missing or invalid bearer token")]
    Unauthorized,
    #[error("{0}")]
    NotFound(String),
    #[error("channel {0} has no playable items")]
    ChannelEmpty(String),
    #[error("item {reported} is not the current item of channel {channel} (it is on {current})")]
    StaleItem {
        channel: String,
        reported: String,
        current: String,
    },
    /// A source the server depends on (a Jellyfin server) failed or could
    /// not be reached.
    #[error("{0}")]
    Upstream(String),
    #[error("internal error")]
    Internal(#[from] Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::Internal(err.into())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        ApiError::Internal(err.into())
    }
}

impl ApiError {
    pub fn code(&self) -> ErrorCode {
        match self {
            ApiError::BadRequest(_) => ErrorCode::BadRequest,
            ApiError::Unauthorized => ErrorCode::Unauthorized,
            ApiError::NotFound(_) => ErrorCode::NotFound,
            ApiError::ChannelEmpty(_) => ErrorCode::ChannelEmpty,
            ApiError::StaleItem { .. } => ErrorCode::StaleItem,
            ApiError::Upstream(_) => ErrorCode::Upstream,
            ApiError::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::ChannelEmpty(_) | ApiError::StaleItem { .. } => StatusCode::CONFLICT,
            ApiError::Upstream(_) => StatusCode::BAD_GATEWAY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Upstream(message) = &self {
            tracing::warn!(%message, "upstream source failed");
        }
        if let ApiError::Internal(err) = &self {
            // Log the full chain; the client only gets a generic message.
            let mut chain = err.to_string();
            let mut source = std::error::Error::source(err);
            while let Some(s) = source {
                chain.push_str(": ");
                chain.push_str(&s.to_string());
                source = s.source();
            }
            tracing::error!(error = %chain, "request failed");
        }
        let mut response = (
            self.status(),
            Json(ErrorBody {
                code: self.code(),
                message: self.to_string(),
            }),
        )
            .into_response();
        if matches!(self, ApiError::Unauthorized) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}
