//! A zone is one Control4 output port (6200, 6201, ...) played out one ALSA device.
//! Each zone runs its own session, so one box serves as many rooms as it has outputs.

use gst::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::AbortHandle;
use tokio_stream::StreamExt;

/// Control4's master clock (audio3clock) port, used when a session names no stream clock.
const NETCLOCK_PORT: u16 = 6002;
/// Announcements arrive on the zone's port + this.
const ANNOUNCEMENT_PORT_OFFSET: u16 = 100;
/// rtpgstpay framing. Caps normally travel in-band; this default (base64 of FLAC 44.1k stereo
/// caps) covers packets that arrive before the in-band caps.
const RTP_CAPS: &str = "application/x-rtp,media=application,clock-rate=90000,encoding-name=X-GST,payload=96,capsversion=0,caps=YXVkaW8veC1mbGFjLCBjaGFubmVscz0oaW50KTIsIGZyYW1lZD0oYm9vbGVhbil0cnVlLCByYXRlPShpbnQpNDQxMDAsIGxheW91dD0oc3RyaW5nKWludGVybGVhdmVkLCBjaGFubmVsLW1hc2s9KGJpdG1hc2spMHgwMDAwMDAwMDAwMDAwMDAz";

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ZoneConfig {
    /// Control4 output port id this zone answers for (6200, 6201, ...).
    pub port: u16,
    pub name: String,
    /// ALSA device, e.g. "hw:1,0" or a name from asound.conf. Ignored if `sink` is set.
    #[serde(default)]
    pub device: Option<String>,
    /// Full GStreamer sink description, overrides `device` (e.g. "fakesink sync=true").
    #[serde(default)]
    pub sink: Option<String>,
    /// Per-output render offset, the knob Control4 keeps in audio_sync.conf.
    #[serde(default)]
    pub offset_ms: i64,
    #[serde(default = "default_max")]
    pub max_volume: u8,
    #[serde(default = "default_volume")]
    pub volume: u8,
}

fn default_max() -> u8 {
    100
}
fn default_volume() -> u8 {
    50
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// 0 play, 2 stop, 4 play announcement, 5 stop announcement.
    pub cmd: i32,
    pub port: u16,
    pub rtcp: u16,
    pub btime: Option<u64>,
    pub sclock: String,
    pub mcast: String,
}

impl Session {
    /// Parses the serialized `session_cmd` GstStructure the streamer sends.
    pub fn parse(s: &str) -> Result<Self, String> {
        // Control4's streamer sends the structure wrapped in double quotes.
        let s = s.trim();
        let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
        let st = gst::Structure::from_str(s).map_err(|_| "session is not a valid structure".to_string())?;
        let int = |k: &str| st.get::<i32>(k).ok();
        let port_of = |k: &str| int(k).and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0);
        let cmd = int("cmd").ok_or("session has no cmd")?;
        let port = port_of("port").ok_or("session port is missing or invalid")?;
        let play = cmd == 0 || cmd == 4;
        let rtcp = if play { port_of("rtcp").ok_or("play session needs an rtcp port")? } else { 0 };
        Ok(Session {
            cmd,
            port,
            rtcp,
            btime: st.get::<u64>("btime").ok().filter(|t| *t != u64::MAX),
            sclock: st.get::<String>("sclock").unwrap_or_default(),
            mcast: st.get::<String>("mcast").unwrap_or_default(),
        })
    }
}

#[derive(Serialize, Clone)]
pub struct View {
    pub port: u16,
    pub name: String,
    pub volume: u8,
    pub muted: bool,
    pub max_volume: u8,
    pub playing: bool,
    pub announcement: bool,
    pub now_playing: BTreeMap<String, String>,
}

struct Playback {
    pipeline: gst::Pipeline,
    bus: AbortHandle,
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.bus.abort();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct Inner {
    volume: u8,
    muted: bool,
    max_volume: u8,
    now_playing: BTreeMap<String, String>,
    /// The regular stream, kept so it can resume after an announcement.
    main: Option<Session>,
    announcement: bool,
    play: Option<Playback>,
    ramp: Option<AbortHandle>,
}

pub struct Zone {
    cfg: ZoneConfig,
    latency_ms: u32,
    inner: Mutex<Inner>,
    events: broadcast::Sender<String>,
}

pub struct Zones {
    map: BTreeMap<u16, Arc<Zone>>,
    events: broadcast::Sender<String>,
}

impl Zones {
    pub fn new(cfgs: Vec<ZoneConfig>, latency_ms: u32) -> Self {
        let (events, _) = broadcast::channel(64);
        let map = cfgs
            .into_iter()
            .map(|cfg| {
                let inner = Inner {
                    volume: cfg.volume.min(100),
                    muted: false,
                    max_volume: cfg.max_volume.min(100),
                    now_playing: BTreeMap::new(),
                    main: None,
                    announcement: false,
                    play: None,
                    ramp: None,
                };
                (cfg.port, Arc::new(Zone { cfg, latency_ms, inner: Mutex::new(inner), events: events.clone() }))
            })
            .collect();
        Zones { map, events }
    }

    pub fn get(&self, port: u16) -> Option<Arc<Zone>> {
        self.map.get(&port).cloned()
    }

    /// Sessions name the zone's port, or port + 100 for an announcement.
    pub fn for_session(&self, port: u16) -> Option<Arc<Zone>> {
        self.get(port).or_else(|| self.get(port.checked_sub(ANNOUNCEMENT_PORT_OFFSET)?))
    }

    pub fn views(&self) -> Vec<View> {
        self.map.values().map(|z| z.view()).collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }
}

impl Zone {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    pub fn view(&self) -> View {
        let i = self.lock();
        self.view_of(&i)
    }

    fn view_of(&self, i: &Inner) -> View {
        View {
            port: self.cfg.port,
            name: self.cfg.name.clone(),
            volume: i.volume,
            muted: i.muted,
            max_volume: i.max_volume,
            playing: i.play.is_some(),
            announcement: i.announcement,
            now_playing: i.now_playing.clone(),
        }
    }

    /// Applies volume to the running pipeline and tells API subscribers.
    fn changed(&self, i: &Inner) {
        if let Some(vol) = i.play.as_ref().and_then(|p| p.pipeline.by_name("vol")) {
            // Cubic taper: 0-100 feels even across the range, a common audio volume curve.
            let linear = f64::from(i.volume.min(i.max_volume)) / 100.0;
            vol.set_property("volume", linear.powi(3));
            vol.set_property("mute", i.muted);
        }
        let _ = self.events.send(serde_json::to_string(&self.view_of(i)).unwrap());
    }

    pub fn set_volume(&self, v: u8) {
        let mut i = self.lock();
        i.volume = v.min(100);
        self.changed(&i);
    }

    pub fn set_muted(&self, m: bool) {
        let mut i = self.lock();
        i.muted = m;
        self.changed(&i);
    }

    pub fn set_max_volume(&self, v: u8) {
        let mut i = self.lock();
        i.max_volume = v.min(100);
        self.changed(&i);
    }

    /// Steps the volume while Control4 holds a volume key; "OFF" (or anything else) stops.
    pub fn ramp(self: &Arc<Self>, dir: &str) {
        let step: i16 = match dir.to_ascii_uppercase().as_str() {
            "UP" => 1,
            "DOWN" => -1,
            _ => 0,
        };
        let mut i = self.lock();
        if let Some(h) = i.ramp.take() {
            h.abort();
        }
        if step == 0 {
            return;
        }
        let z = self.clone();
        i.ramp = Some(
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let v = i16::from(z.view().volume) + step;
                    if !(0..=100).contains(&v) {
                        break;
                    }
                    z.set_volume(v as u8);
                }
            })
            .abort_handle(),
        );
    }

    /// Runs a `session` command. Blocking (pipeline start-up and clock sync): call off the runtime.
    pub fn session(self: &Arc<Self>, s: Session, peer: IpAddr) -> Result<(), String> {
        let resume = match s.cmd {
            0 => {
                let mut i = self.lock();
                i.main = Some(s.clone());
                if i.announcement {
                    return Ok(()); // starts when the announcement ends
                }
                Some(s)
            }
            4 => {
                self.lock().announcement = true;
                Some(s)
            }
            2 => {
                let mut i = self.lock();
                i.main = None;
                if !i.announcement {
                    i.play = None;
                    i.now_playing.clear();
                }
                None
            }
            5 => {
                let mut i = self.lock();
                i.announcement = false;
                i.play = None;
                i.main.clone()
            }
            _ => return Err("unknown session cmd".into()),
        };
        match resume {
            Some(s) => self.start(&s, peer),
            None => {
                self.changed(&self.lock());
                Ok(())
            }
        }
    }

    fn describe(&self, s: &Session, peer: IpAddr) -> String {
        let sink = self.cfg.sink.clone().unwrap_or_else(|| {
            format!("alsasink device={}", self.cfg.device.as_deref().unwrap_or("default"))
        });
        // ponytail: RTP on the session port (or the mcast group) and no RTCP-in socket; the
        // exact socket layout is unconfirmed until a capture (see PROTOCOL.md).
        let (addr, rtp_port) = match s.mcast.rsplit_once(':').and_then(|(g, p)| Some((g, p.parse::<u16>().ok()?))) {
            Some((group, port)) => (format!("address={group} "), port),
            None => (String::new(), s.port),
        };
        format!(
            "rtpbin name=rb latency={lat} buffer-mode=synced do-retransmission=true rtp-profile=avpf \
             udpsrc {addr}port={rtp_port} caps=\"{RTP_CAPS}\" ! rb.recv_rtp_sink_0 \
             rb.send_rtcp_src_0 ! udpsink host={peer} port={rtcp} sync=false async=false \
             rb. ! rtpgstdepay name=depay ! flacparse ! flacdec ! audioconvert ! audioresample ! volume name=vol ! {sink} name=out",
            lat = self.latency_ms,
            rtcp = s.rtcp,
        )
    }

    fn start(self: &Arc<Self>, s: &Session, peer: IpAddr) -> Result<(), String> {
        // The old pipeline must release the ALSA device before the new one opens it.
        self.lock().play = None;

        let pipeline = gst::parse::launch(&self.describe(s, peer))
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
        if let Some(out) = pipeline.by_name("out").filter(|o| o.find_property("ts-offset").is_some()) {
            out.set_property("ts-offset", self.cfg.offset_ms * 1_000_000);
        }

        // Play on Control4's clock. btime is a base time on the clock the streamer named
        // WITH it, so it is only valid paired with that exact clock — never carried onto a
        // fallback (doing so leaves the sink unable to map running-time and it never prerolls).
        //  - Dictated stream clock (sclock, native flow): try ONLY it, briefly. If it syncs,
        //    use it and apply btime. If it does not (e.g. a per-stream provider that a raw
        //    addcli never brought up — seen live as .139:6008 answering nothing), DROP btime
        //    and play on the pipeline's own clock: unsynced but audible, not stalled.
        //  - No dictated clock: fall back to the master net clock on the streamer's host.
        let try_sync = |host: &str, port: u16, timeout: gst::ClockTime| {
            let clock = gst_net::NetClientClock::new(None, host, i32::from(port), gst::ClockTime::ZERO);
            let ok = clock.wait_for_sync(timeout).is_ok();
            eprintln!("zone {}: clock {host}:{port} {}", self.cfg.port, if ok { "synced" } else { "did not sync" });
            ok.then_some(clock)
        };
        let dictated = s.sclock.rsplit_once(':').and_then(|(h, p)| Some((h.to_string(), p.parse::<u16>().ok()?)));
        match dictated {
            // Short timeout: a live provider syncs fast on a LAN, and a dead one must not
            // stall preroll — we want audible playback quickly either way.
            Some((host, port)) => match try_sync(&host, port, gst::ClockTime::from_mseconds(1500)) {
                Some(clock) => {
                    pipeline.use_clock(Some(&clock));
                    if let Some(b) = s.btime {
                        pipeline.set_base_time(gst::ClockTime::from_nseconds(b));
                        pipeline.set_start_time(gst::ClockTime::NONE);
                    }
                }
                None => eprintln!(
                    "zone {}: dictated clock {host}:{port} dead; playing unsynchronised (btime dropped)",
                    self.cfg.port
                ),
            },
            None => match try_sync(&peer.to_string(), NETCLOCK_PORT, gst::ClockTime::from_seconds(3)) {
                Some(clock) => pipeline.use_clock(Some(&clock)),
                None => eprintln!("zone {}: no Control4 clock; playing unsynchronised", self.cfg.port),
            },
        }

        if let Some(pad) = pipeline.by_name("depay").and_then(|d| d.static_pad("sink")) {
            pad.add_probe(gst::PadProbeType::BUFFER, |_, info| {
                if let Some(gst::PadProbeData::Buffer(buf)) = &mut info.data {
                    let fixed = buf.map_readable().ok().and_then(|m| strip_c4_timestamp(&m));
                    if let Some(bytes) = fixed {
                        let mut nb = gst::Buffer::from_mut_slice(bytes);
                        let w = nb.get_mut().expect("new buffer is writable");
                        w.set_pts(buf.pts());
                        w.set_dts(buf.dts());
                        w.set_flags(buf.flags());
                        *buf = nb;
                    }
                }
                gst::PadProbeReturn::Ok
            });
        }

        let bus = self.watch(pipeline.bus().expect("pipeline has a bus"));
        pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        let mut i = self.lock();
        i.now_playing.clear();
        i.play = Some(Playback { pipeline, bus });
        self.changed(&i);
        Ok(())
    }

    /// Tags (title/artist/...) that ride in-band through rtpgstpay become now-playing.
    fn watch(self: &Arc<Self>, bus: gst::Bus) -> AbortHandle {
        let zone: Weak<Zone> = Arc::downgrade(self);
        let mut msgs = bus.stream();
        tokio::runtime::Handle::current()
            .spawn(async move {
                while let Some(msg) = msgs.next().await {
                    let Some(z) = zone.upgrade() else { break };
                    match msg.view() {
                        gst::MessageView::Tag(t) => {
                            let mut i = z.lock();
                            for (name, value) in t.tags().iter() {
                                // Strings, numbers, dates; cover-art samples don't transform and are skipped.
                                if let Some(s) = value.transform::<String>().ok().and_then(|v| v.get::<String>().ok()) {
                                    i.now_playing.insert(name.to_string(), s);
                                }
                            }
                            z.changed(&i);
                        }
                        gst::MessageView::Error(e) => {
                            eprintln!("zone {}: {} ({:?})", z.cfg.port, e.error(), e.debug());
                            let mut i = z.lock();
                            i.play = None; // aborts this task too
                            z.changed(&i);
                        }
                        _ => {}
                    }
                }
            })
            .abort_handle()
    }
}

/// Control4's streamer marks the first fragment of each frame with bit 0 of byte 2 of
/// the `rtpgstpay` header and puts an 8-byte big-endian timestamp (ns) between the header
/// and the data; its fragment offsets do not count those 8 bytes. Stock `rtpgstdepay`
/// knows neither, so it drops every frame. Returns the packet in stock framing, or `None`
/// if it is already stock.
// ponytail: the timestamp is discarded; RTP timestamps carry the timing. Use it if
// multi-room alignment turns out to need it.
fn strip_c4_timestamp(pkt: &[u8]) -> Option<Vec<u8>> {
    let cc = usize::from(*pkt.first()? & 0x0f);
    let mut p = 12 + 4 * cc;
    if pkt[0] & 0x10 != 0 {
        let words = u16::from_be_bytes([*pkt.get(p + 2)?, *pkt.get(p + 3)?]);
        p += 4 + 4 * usize::from(words);
    }
    if *pkt.get(p + 2)? & 0x01 == 0 || pkt.len() < p + 16 {
        return None;
    }
    let mut out = Vec::with_capacity(pkt.len() - 8);
    out.extend_from_slice(&pkt[..p + 8]);
    out[p + 2] &= !0x01;
    out.extend_from_slice(&pkt[p + 16..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control4_frame_timestamp() {
        // First fragment of a frame from a live Control4 OS 3.3 streamer (first 40 bytes).
        let hex = "8060b356c8f7ccaa7302db43000001000000000000000338c429f624fff85a8ce980a0e11807e63a";
        let pkt: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        let out = strip_c4_timestamp(&pkt).unwrap();
        assert_eq!(out.len(), pkt.len() - 8);
        assert_eq!(&out[..12], &pkt[..12], "RTP header untouched");
        assert_eq!(&out[12..20], &[0, 0, 0, 0, 0, 0, 0, 0], "flag cleared, frag offset 0");
        assert_eq!(&out[20..22], &[0xff, 0xf8], "FLAC frame sync follows the header");
        // Later fragments (and stock rtpgstpay) pass through.
        assert!(strip_c4_timestamp(&out).is_none());
    }

    #[test]
    fn parses_session_cmd() {
        gst::init().unwrap();
        let s = Session::parse(
            "session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)7001, btime=(guint64)123456789, sclock=(string)\"10.0.0.2:7002\", mcast=(string)\"239.1.1.1:7000\";",
        )
        .unwrap();
        assert_eq!((s.cmd, s.port, s.rtcp, s.btime), (0, 6200, 7001, Some(123456789)));
        assert_eq!((s.sclock.as_str(), s.mcast.as_str()), ("10.0.0.2:7002", "239.1.1.1:7000"));

        // Verbatim from a live Control4 OS 3.3 streamer.
        let live = Session::parse(
            "\"session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)6003, btime=(guint64)18446744073709551615, sclock=(string)10.0.0.2:6004;\"",
        )
        .unwrap();
        assert_eq!((live.port, live.rtcp, live.btime, live.sclock.as_str()), (6200, 6003, None, "10.0.0.2:6004"));

        let stop = Session::parse("session_cmd, cmd=(int)2, port=(int)6201").unwrap();
        assert_eq!((stop.cmd, stop.port, stop.rtcp, stop.btime), (2, 6201, 0, None));

        assert!(Session::parse("session_cmd, cmd=(int)0, port=(int)6200").is_err(), "play needs rtcp");
        assert!(Session::parse("nonsense ,,").is_err());
    }
}
