pub mod games;
pub mod players;
pub mod predict;
pub mod teams;

use crate::AppState;
use axum::Router;
use std::sync::Arc;

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(teams::router())
        .merge(players::router())
        .merge(games::router())
        .merge(predict::router())
}
