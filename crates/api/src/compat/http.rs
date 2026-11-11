//! The compat HTTP API: `GET /api` gives the XML state; `GET /api/?Function=…&…` calls a
//! function: 200 if it is done, 500 with the reason if not (`api-compat.md` § 1).

use axum::Router;
use axum::extract::{RawQuery, State as Extract};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::{Call, resolve, state_xml};
use crate::{Shared, run};

pub(crate) fn router(shared: Shared) -> Router {
    // The paths as the reference's examples write them, any case.
    Router::new()
        .route("/api", get(api))
        .route("/api/", get(api))
        .route("/API", get(api))
        .route("/API/", get(api))
        .with_state(shared)
}

async fn api(Extract(shared): Extract<Shared>, RawQuery(query): RawQuery) -> Response {
    let Some(call) = query.as_deref().and_then(Call::from_query) else {
        let xml = state_xml(&shared.state.load());
        return (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/xml; charset=utf-8")],
            xml,
        )
            .into_response();
    };
    match call_function(&shared, &call).await {
        Ok(()) => (StatusCode::OK, "Function completed successfully.").into_response(),
        Err(reason) => (StatusCode::INTERNAL_SERVER_ERROR, reason).into_response(),
    }
}

/// Resolves `call` on the state now and runs its commands in order; the first refusal stops.
pub(crate) async fn call_function(shared: &Shared, call: &Call) -> Result<(), String> {
    let commands = resolve(call, &shared.state.load()).map_err(|e| e.to_string())?;
    for command in commands {
        run(shared, command).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}
