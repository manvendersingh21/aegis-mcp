//! Agent credential resolution (PHASE6.md §3). Stub — owned by the HACP worker.
use crate::BrokerState;
use axum::Router;

pub fn resolve_router(_state: BrokerState) -> Router {
    Router::new()
}
