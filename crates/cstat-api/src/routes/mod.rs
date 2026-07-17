pub mod alert_selftest;
pub mod archetypes;
pub mod client_error;
pub mod coaches;
pub mod draft;
pub mod games;
pub mod health;
pub mod lineups;
pub mod meta;
pub mod og_image;
pub mod players;
pub mod portle;
pub mod predict;
pub mod projections;
pub mod recruits;
pub mod seasons;
pub mod sitemap;
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
        .merge(portle::router())
        .merge(lineups::router())
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
    // NOTE: client_error::router() is deliberately NOT merged here — it is
    // mounted un-guarded in main.rs (alongside the health routes) so a browser
    // error-storm can't consume the data routes' load-shed budget and 503 real
    // reads. See `client_error`'s module docs.
}
