# control4-audio-client

An open implementation of Control4 OS3's digital audio client (`audio3client`).
Control4 streams to it natively, as it does to its own controllers and amps. It runs
on any Linux box: each ALSA output is a zone, and a local HTTP API exposes
state, volume and now-playing to whatever manages the box.

- **Multi-zone.** Every Control4 output port (6200, 6201, ...) gets its own session
  and pipeline, so one box can play different sources in different rooms or the
  same source in sync.
- **Synchronised.** It plays on Control4's network clock at the stream's base time,
  using standard GStreamer network-synchronised playback, so it stays in step with
  Control4 endpoints in other rooms.
- **Now-playing.** Tags carried in the stream (title, artist, album, ...) show up
  per zone.
- **Controllable.** Control4 sets volume, mute and ramp over its own channel. The
  HTTP API sets the same values locally and pushes every change as an event.

How the protocol works: [PROTOCOL.md](PROTOCOL.md).

> **Unofficial.** This is an independent project, not affiliated with, endorsed by
> or supported by Control4 or its owner. "Control4" and related names are
> trademarks of their respective owner and are used here only to say what this
> software works with. The protocol was documented for interoperability; no
> Control4 software, files or code are included. Use it on systems you own or
> manage, at your own risk.

## Run

Needs GStreamer 1.14+ with plugins-base, plugins-good and alsa
(Debian: `gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-alsa`).

```bash
cargo build --release
sudo install -m755 target/release/c4-audio-client /usr/bin/
sudo install -m644 c4-audio-client.toml /etc/
sudo install -m644 c4-audio-client.service /etc/systemd/system/
sudo systemctl enable --now c4-audio-client
```

Edit `/etc/c4-audio-client.toml` with one `[[zone]]` per output. `curl
localhost:6180/api/devices` lists the ALSA devices.

### One multichannel card, several zones

Split the card in `/etc/asound.conf` and give each zone its own pair of
channels. Here an 8-channel card becomes four stereo zones:

```
pcm.card8 { type hw; card 0 }
pcm.zone1 { type plug; slave.pcm { type dshare; ipc_key 4242; slave { pcm card8; channels 8 }; bindings [ 0 1 ] } }
pcm.zone2 { type plug; slave.pcm { type dshare; ipc_key 4242; slave { pcm card8; channels 8 }; bindings [ 2 3 ] } }
pcm.zone3 { type plug; slave.pcm { type dshare; ipc_key 4242; slave { pcm card8; channels 8 }; bindings [ 4 5 ] } }
pcm.zone4 { type plug; slave.pcm { type dshare; ipc_key 4242; slave { pcm card8; channels 8 }; bindings [ 6 7 ] } }
```

Then set `device = "zone1"` ... `"zone4"` in the config.

## HTTP API

Default bind is `127.0.0.1:6180`. The API has no authentication, so keep it on
localhost or a trusted network.

| | |
|---|---|
| `GET /api/zones` | every zone: `port, name, volume, muted, max_volume, playing, announcement, now_playing{}` |
| `GET /api/zones/{port}` | one zone |
| `PATCH /api/zones/{port}` | `{"volume": 0-100, "muted": bool, "max_volume": 0-100}`, any subset |
| `GET /api/events` | server-sent events, one zone JSON per change |
| `GET /api/devices` | `/proc/asound/pcm` |

## Getting Control4 to stream to it

Control4 streams to a project device whose driver has:
- a `DIGITAL_AUDIO_CLIENT` input,
- audio outputs, where output *N* is client zone 6200+*N* (the same order Control4's
  own controllers use), and
- a TCP network port named `Audioclient` on 6100.

`driver/` is a demo driver with that shape: one input, four outputs (zones
6200–6203), and room-selection connections that carry room volume and mute to the
client. Build it with `driver/package.sh` (check it with `cd driver && lua test.lua`).

**Discovery.** The client announces itself over SDDP as `c4ac:audio_client`, the
driver's `search_type`. Add the driver in Composer Pro, and Director binds its IP
Connection to the box by itself; the driver's **Status** property shows the address
once it connects. If SDDP can't cross your network, set the address on Composer's
Network tab. Turn announcements off with `sddp = false`.

Then:

1. Bind the driver's **Digital Audio Client** input to Digital Media (it usually
   binds itself).
2. Bind **Room Selection *N*** to a room, or **Audio Output *N*** to that room's
   speakers.
3. Play something from Digital Media in that room.

The client logs every control line it receives, so `c4-audio-client`'s output shows
exactly what Director sends. **Last Proxy Command**, **Last Command** and **Last
Reply** in the driver's properties show the other side.

## Run on a Mac (for testing)

```bash
brew install gstreamer
cat > mac.toml <<'EOF'
api = "0.0.0.0:6180"
[[zone]]
port = 6200
name = "Mac 1"
sink = "osxaudiosink"
[[zone]]
port = 6201
name = "Mac 2"
sink = "osxaudiosink"
EOF
cargo run --release -- mac.toml
```

Allow incoming connections when macOS asks: Director has to reach TCP 6100 and
the UDP RTP ports.

## Test

```bash
docker build -t c4ac . && docker run --rm c4ac
```

This builds the client, runs the unit tests (session parsing, SDDP), then runs `test/selfcheck.py`. The
self-check acts as Control4's streamer: it serves a network clock, sends tagged
FLAC over RTP, drives the control channel and checks the result through the API.

## Status

Built from a reverse-engineered protocol and checked against a simulated
streamer, not yet against a live Control4 system. The open questions are listed
at the end of PROTOCOL.md. Not implemented yet: retransmission (RTX), HDMI and
passthrough parameters, advanced-announcement file playback (`prepadvancmnt`),
and keeping volume across restarts.
