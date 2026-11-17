//! Local HTTP API for whatever manages the box: zone state, volume/mute, now-playing, live events.

use crate::zone::{View, Zones};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

pub async fn serve(addr: String, zones: Arc<Zones>) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/zones", get(|State(z): State<Arc<Zones>>| async move { Json(z.views()) }))
        .route("/api/zones/:port", get(one).patch(update))
        .route("/api/events", get(events))
        // ALSA's own list of playback/capture devices, to pick `device` values from.
        .route("/api/devices", get(|| async { std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default() }))
        .with_state(zones);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("api on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn one(State(z): State<Arc<Zones>>, Path(port): Path<u16>) -> Result<Json<View>, StatusCode> {
    z.get(port).map(|z| Json(z.view())).ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    volume: Option<u8>,
    muted: Option<bool>,
    max_volume: Option<u8>,
}

async fn update(
    State(z): State<Arc<Zones>>,
    Path(port): Path<u16>,
    Json(p): Json<Patch>,
) -> Result<Json<View>, StatusCode> {
    let zone = z.get(port).ok_or(StatusCode::NOT_FOUND)?;
    if [p.volume, p.max_volume].iter().flatten().any(|v| *v > 100) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    if let Some(v) = p.max_volume {
        zone.set_max_volume(v);
    }
    if let Some(v) = p.volume {
        zone.set_volume(v);
    }
    if let Some(m) = p.muted {
        zone.set_muted(m);
    }
    Ok(Json(zone.view()))
}

/// Server-sent events: one JSON zone view per change (volume, mute, play/stop, new tags).
async fn events(State(z): State<Arc<Zones>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(z.subscribe()).filter_map(|m| m.ok().map(|s| Ok(Event::default().data(s))));
    Sse::new(stream).keep_alive(KeepAlive::default())
}
