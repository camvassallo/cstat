pub mod archetypes;
pub mod coaches;
pub mod draft;
pub mod games;
pub mod players;
pub mod predict;
pub mod projections;
pub mod recruits;
pub mod seasons;
pub mod teams;
pub mod ticker;
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
        .merge(coaches::router())
        .merge(seasons::router())
        .merge(ticker::router())
        .merge(transfers::router())
        .merge(projections::router())
        .merge(recruits::router())
        .merge(draft::router())
}
