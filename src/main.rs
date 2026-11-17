mod api;
mod control;
mod zone;

use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// Where Control4's streamer connects. Must be port 6100 for a real system.
    #[serde(default = "default_control")]
    control: String,
    /// Local HTTP API for whatever manages the box.
    #[serde(default = "default_api")]
    api: String,
    /// rtpbin jitter-buffer latency; Control4's own default is 1200 (300..3000).
    #[serde(default = "default_latency")]
    latency_ms: u32,
    /// Announce over SDDP so Composer discovers the box and binds the driver to it.
    #[serde(default = "default_sddp")]
    sddp: bool,
    /// Name shown in Composer's discovery list. Defaults to the hostname.
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "zone")]
    zones: Vec<zone::ZoneConfig>,
}

fn default_control() -> String {
    "0.0.0.0:6100".into()
}
fn default_api() -> String {
    "127.0.0.1:6180".into()
}
fn default_latency() -> u32 {
    1200
}
fn default_sddp() -> bool {
    true
}

/// SDDP Type the driver's `<search_types>` lists (driver/driver.xml).
const SDDP_TYPE: &str = "c4ac:audio_client";

/// Announce over SDDP so Director pairs the box with the driver and fills in its address.
async fn announce(host: String) -> Option<sddp::Announcer> {
    let started = sddp::Announcer::builder(host.clone(), SDDP_TYPE)
        .primary_proxy("avswitch")
        .proxies(["avswitch"])
        .manufacturer("Open Source")
        .model("c4-audio-client")
        .driver("c4-audio-client.c4z")
        .start()
        .await;
    match started {
        Ok(a) => {
            eprintln!("sddp announcing {SDDP_TYPE} as {host} on {}", a.interface());
            Some(a)
        }
        Err(e) => {
            eprintln!("sddp disabled: {e}");
            None
        }
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "c4-audio-client".into())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/etc/c4-audio-client.toml".into());
    let cfg: Config = toml::from_str(&std::fs::read_to_string(&path)?)?;
    anyhow::ensure!(!cfg.zones.is_empty(), "{path}: no [[zone]] configured");
    gst::init()?;
    // Kept alive for the life of the process; dropping it sends NOTIFY OFFLINE.
    let _announcer = if cfg.sddp { announce(cfg.name.unwrap_or_else(hostname)).await } else { None };
    let zones = Arc::new(zone::Zones::new(cfg.zones, cfg.latency_ms));
    tokio::select! {
        r = control::serve(cfg.control, zones.clone()) => r,
        r = api::serve(cfg.api, zones) => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
