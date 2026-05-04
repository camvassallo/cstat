pub mod archetypes;
pub mod games;
pub mod players;
pub mod predict;
pub mod seasons;
pub mod teams;
pub mod transfers;

use crate::AppState;
use axum::Router;
use std::sync::Arc;

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(teams::router())
        .merge(players::router())
        .merge(games::router())
        .merge(predict::router())
        .merge(archetypes::router())
        .merge(seasons::router())
        .merge(transfers::router())
}
