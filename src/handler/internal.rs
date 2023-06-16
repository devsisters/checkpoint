use axum::{routing, Router};

use super::AppState;

pub fn create_router() -> Router<AppState> {
    Router::new().route(
        "/mutate/cronpolicies",
        routing::post(post_mutate_cronpolicy),
    )
}

async fn post_mutate_cronpolicy() {
    todo!()
}
