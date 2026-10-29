//! Chess routes, for the unlisted `/chess` page. See [`crate::chess`].
//!
//! These are the web app's side only — the other player's daemon never calls
//! them. It talks to [`crate::chess::lan`]'s own listener instead, which speaks
//! nothing but the game protocol.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::chess::game::{Action, Rules};
use crate::chess::lan::{GameView, Lobby};
use crate::daemon::app::AppState;

use super::{RouteError, RouteResult};

/// How long `GET /api/chess/games/{id}?since=` waits for something to change.
const LONG_POLL: Duration = Duration::from_secs(25);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/chess", get(lobby))
        .route("/api/chess/name", post(set_name))
        .route("/api/chess/games", post(create))
        .route("/api/chess/games/{id}", get(game).delete(remove))
        .route("/api/chess/games/{id}/action", post(act))
        .route("/api/chess/browse", post(browse))
        .route("/api/chess/join", post(join))
}

async fn lobby(State(state): State<AppState>) -> Json<Lobby> {
    Json(state.chess.lobby().await)
}

#[derive(Deserialize)]
struct NamePayload {
    name: String,
}

async fn set_name(
    State(state): State<AppState>,
    Json(payload): Json<NamePayload>,
) -> RouteResult<Json<serde_json::Value>> {
    let name = state
        .chess
        .set_name(&payload.name)
        .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "name": name })))
}

async fn create(
    State(state): State<AppState>,
    Json(rules): Json<Rules>,
) -> RouteResult<Json<GameView>> {
    let view = state
        .chess
        .create(rules)
        .await
        .map_err(RouteError::bad_request)?;
    Ok(Json(view))
}

#[derive(Deserialize)]
struct Since {
    since: Option<u64>,
}

/// One game. With `?since=<rev>`, waits until the game's `rev` differs from
/// that (or [`LONG_POLL`] passes), so the page hears moves as they happen.
async fn game(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<Since>,
) -> RouteResult<Json<GameView>> {
    let mut changes = state.chess.subscribe();
    let deadline = tokio::time::Instant::now() + LONG_POLL;
    loop {
        let view = state
            .chess
            .view(&id)
            .ok_or_else(|| RouteError::not_found(format!("No game '{id}'")))?;
        if query.since.is_none_or(|since| view.rev != since) || state.chess.is_closing() {
            return Ok(Json(view));
        }
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    return Ok(Json(view));
                }
            }
            _ = tokio::time::sleep_until(deadline) => return Ok(Json(view)),
        }
    }
}

async fn act(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(action): Json<Action>,
) -> RouteResult<Json<GameView>> {
    let view = state
        .chess
        .act(&id, action)
        .await
        .map_err(RouteError::bad_request)?;
    Ok(Json(view))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> RouteResult<Json<serde_json::Value>> {
    state
        .chess
        .remove(&id)
        .await
        .map_err(RouteError::not_found)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AddressPayload {
    address: String,
}

async fn browse(
    State(state): State<AppState>,
    Json(payload): Json<AddressPayload>,
) -> RouteResult<Json<serde_json::Value>> {
    let (name, games) = state
        .chess
        .browse(payload.address.trim())
        .await
        .map_err(RouteError::bad_request)?;
    Ok(Json(
        json!({ "name": name, "address": payload.address.trim(), "games": games }),
    ))
}

#[derive(Deserialize)]
struct JoinPayload {
    address: String,
    game: String,
}

async fn join(
    State(state): State<AppState>,
    Json(payload): Json<JoinPayload>,
) -> RouteResult<Json<GameView>> {
    let view = state
        .chess
        .join(&payload.address, &payload.game)
        .await
        .map_err(RouteError::bad_request)?;
    Ok(Json(view))
}
