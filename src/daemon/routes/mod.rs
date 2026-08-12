//! Feature routes, one module per web app the daemon replaced.
//!
//! Each module is deliberately thin: the domain logic already lives in
//! `crate::{todo, watch, run, analyze, ai}`, and only the transport
//! changed when those hand-rolled servers were folded into the daemon.

pub mod ai;
pub mod analyze;
pub mod projects;
pub mod run;
pub mod todo;
pub mod watch;
pub mod workspace;

use axum::Router;

use super::app::AppState;

/// All authenticated feature routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(ai::router())
        .merge(analyze::router())
        .merge(run::router())
        .merge(projects::router())
        .merge(todo::router())
        .merge(watch::router())
        .merge(workspace::router())
}

/// Shared error type for route handlers, so they can use `?` on `anyhow`
/// errors and still return a sensible HTTP response.
///
/// Anything that isn't explicitly a client error becomes a 500 with the error
/// chain as the body — these are local-only tools and the operator is the
/// developer, so a useful message beats a generic one.
#[derive(Debug)]
pub struct RouteError {
    pub status: axum::http::StatusCode,
    pub message: String,
    /// Extra fields merged into the JSON body alongside `error`, for the cases
    /// where the web app has to act on the failure rather than just show it
    /// (see [`RouteError::missing_env`]).
    pub details: Option<serde_json::Value>,
}

impl RouteError {
    pub fn new(status: axum::http::StatusCode, message: impl std::fmt::Display) -> Self {
        Self {
            status,
            message: message.to_string(),
            details: None,
        }
    }

    pub fn bad_request(message: impl std::fmt::Display) -> Self {
        Self::new(axum::http::StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl std::fmt::Display) -> Self {
        Self::new(axum::http::StatusCode::NOT_FOUND, message)
    }

    /// A run can't start until these variables are supplied. Carries the names
    /// as `missing_env` so the launcher can prompt for them and retry, rather
    /// than only printing the message.
    pub fn missing_env(vars: &[String]) -> Self {
        Self {
            status: axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            message: format!(
                "Missing required environment variable(s): {}.",
                vars.join(", ")
            ),
            details: Some(serde_json::json!({ "missing_env": vars })),
        }
    }
}

impl From<anyhow::Error> for RouteError {
    fn from(e: anyhow::Error) -> Self {
        Self::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("{e:#}"),
        )
    }
}

impl axum::response::IntoResponse for RouteError {
    fn into_response(self) -> axum::response::Response {
        let mut body = serde_json::json!({ "error": self.message });
        if let Some(serde_json::Value::Object(extra)) = self.details {
            for (key, value) in extra {
                body[key] = value;
            }
        }
        (self.status, axum::Json(body)).into_response()
    }
}

/// Result alias for route handlers.
pub type RouteResult<T> = std::result::Result<T, RouteError>;
