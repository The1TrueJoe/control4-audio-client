# Control4 OS3 digital audio: the client interface

This describes the network interface between a Control4 OS3 system and a digital
audio endpoint: what crosses the wire, and what a driver must declare so Director
streams to the endpoint. It was worked out for interoperability, by observing
Control4 equipment the author owns. It covers only the interface needed to build a
compatible endpoint. No Control4 software, files or code are included in this repo.

## Pieces

| Role | Runs on | Port |
|---|---|---|
| Digital Media (decides what plays where) | Director | — |
| Audio stream server | Director's host | TCP 6000 |
| Per-stream streamer: source → FLAC → RTP; connects *to* each endpoint | Director's host | per-stream RTCP and clock ports |
| Master network clock (`GstNetTimeProvider`) | Director's host | UDP 6002 |
| **Endpoint** (this project) | each audio device | **TCP 6100** |

The control channels are plain TCP with CRLF line endings, no TLS.

```
Digital Media ──(adds the endpoint to a stream)──▶ stream server :6000
stream server ──starts──▶ streamer
streamer ──TCP──▶ endpoint:6100      "<tag> session session_cmd, cmd=0, ..."
streamer ──RTP/UDP (FLAC in rtpgstpay)──▶ endpoint
endpoint ──RTCP──▶ streamer:<rtcp>
endpoint ──network clock──▶ sclock (stream clock), or Director's host :6002
```

## Control channel (endpoint listens on TCP 6100)

Tagged lines, in the style of IMAP:

```
-> <tag> <command> [args...]\r\n        args separated by single spaces
<- <tag> <command> <data>\r\n          zero or more data lines
<- <tag> OK <command>\r\n              success
<- <tag> NO <command>\r\n              understood, but failed (the data line says why)
<- <tag> BAD <command> <message>\r\n   unknown command or wrong arguments
<- * SYS <EVENT>=...\r\n               unsolicited event
```

For example, from a live stream server: `q1 status` is answered by
`q1 status Audio Server 3.3.0...` and then `q1 OK status`; `q1 ping` by `q1 OK ping`.
The stream server (TCP 6000) and the endpoint (TCP 6100) use the same convention.

Commands are case-insensitive. The streamer sends `PING` as a keepalive and expects
a reply within about 10 s. Message text after `OK`/`NO`/`BAD` is free-form.

### Commands

| cmd | args | notes |
|---|---|---|
| `session` | one serialized GstStructure | **the important one**, see below |
| `addch` / `remch` | `PORT` | add/remove an output channel (port = output id, 6200+) |
| `setvol` | `PORT LEFT RIGHT` | 0–100 |
| `getvol` | `PORT` | replies with the level |
| `setmut` | `PORT STATE` | `ON`/`OFF` |
| `getmut` / `togmut` | `PORT` | |
| `setmaxvol` / `getmaxvol` | `PORT VALUE` / `PORT` | |
| `ramp` | `PORT DIR` | `UP`/`DOWN`/`OFF` |
| `getparam` / `setparam` | `[PORT]:KEY [VALUE]` | e.g. `latency`, `latency_profile` |
| `listparams`, `status`, `vers`, `reset`, `initialize` | | |
| `prepadvancmnt` / `playadvancmnt` / `finadvancmnt` | | timed announcements; progress via `* SYS ADVANCMNT=...` |

A minimal endpoint needs `session` and `PING`, and should answer the volume/mute
commands. Replying `OK` to the rest is enough.

### `session`

The argument is a serialized GstStructure wrapped in double quotes. Verbatim from a
live controller:

```
4 session "session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)6003, btime=(guint64)18446744073709551615, sclock=(string)10.0.0.2:6004;"
```

`btime` of all ones means no base time. `sclock` is `host:port`. An optional
`mcast=(string)"<group>:<port>"` selects multicast.

| `cmd` | meaning | fields |
|---|---|---|
| 0 | play stream | port, rtcp, btime, sclock, [mcast] |
| 2 | stop stream | port |
| 4 | play announcement | as 0 |
| 5 | stop announcement | port |

A play for a port that is already playing replaces the old stream. The endpoint
answers with the data line `<tag> session session_reply;` (an empty `session_reply`
structure) and then `<tag> OK session`.

The streamer sends `PING` about every 10 s on the same connection.

## Media

- **Codec:** FLAC, stereo, 44.1–192 kHz, 16/24 bit.
- **Payload:** GStreamer's `rtpgstpay` (payload type 96, `encoding-name=X-GST`,
  clock-rate 90000), **with a Control4 extension**: the first fragment of each frame
  sets bit 0 of byte 2 of the 8-byte `rtpgstpay` header (a field stock GStreamer
  keeps zero) and carries an 8-byte big-endian timestamp in nanoseconds between the
  header and the data. The fragment offsets do not count those 8 bytes. Stock
  `rtpgstdepay` rejects every such frame, so strip the 8 bytes and clear the bit
  first, then `rtpgstdepay ! flacparse ! flacdec`. Frames are about 96 ms, sent as
  1380-byte packets.
- **RTP session:** `rtpbin` with `rtp-profile=avpf`, retransmission requests on,
  `buffer-mode=synced`, latency around 1200 ms (300–3000). RTCP goes to the
  streamer's `rtcp` port.
- **Multicast:** optional, when the session carries `mcast`.
- **Announcements:** arrive on the zone's port + 100.

## Now-playing metadata

Source metadata (title, artist, album) rides in the RTP stream as GStreamer tag
events: `rtpgstpay` serializes them, `rtpgstdepay` re-emits them, and they show up as
tag messages on the receiving pipeline's bus. Cover art and Director's own
now-playing information (service, queue) are not in the stream.

## Clock and sync

Standard GStreamer network-synchronised playback:

- The clock protocol is `GstNetTimeProvider`. Requests are the standard 16 bytes
  (two big-endian u64 nanosecond timestamps); Control4's master clock answers with
  20: the same 16 plus a 4-byte trailer. Stock `GstNetClientClock` syncs to it.
- Control4's streamers (`audio3streamer --master 127.0.0.1`) and clients all slave to
  the master `audio3clock` on port 6002 (PID-tuned by `/etc/clock_sync.conf`). The
  per-stream `sclock` a session names is never served on an EA controller — no
  per-stream `audio3clock` is ever spawned — and `btime` is on the master timeline
  (it tracks the controller's uptime).
- So the endpoint sets its pipeline clock to a `GstNetClientClock` on the streamer's
  host :6002 (`sclock` only if that doesn't answer), sets the base time to `btime` and
  the start time to none, and plays once the clock is synced. That is what keeps rooms
  in sync.
- Each output needs a small render-offset adjustment to line up with other rooms;
  this client exposes it as `offset_ms` per zone.

## What a driver must declare

Director streams to a project device whose driver declares:

- a network connection with a TCP port named `Audioclient`, number 6100;
- one consumer connection of class `DIGITAL_AUDIO_CLIENT` (type 6, autobind),
  which binds to Digital Media's digital audio output;
- its audio output connections. **The endpoint port is 6200 + the output's index**,
  counting the device's audio outputs in order (three outputs: 6200–6202).

`driver/` is an example. Still unconfirmed: whether Director treats a third-party
driver with this shape the same way as Control4's own devices.

## The stream server (TCP 6000)

Same line protocol. `help` lists its commands, including `getstr`, `getsta SID`,
`addstr`, `addmed SID URL`, `setsta SID PLAY|PAUSE`, `addcli SID HOST PORT`,
`remcli SID HOST PORT` and `status`. `addcli` makes the stream's streamer connect to
`HOST:6100`, send `session` for `PORT`, and send RTP to `HOST:PORT`, which is a
handy way to test an endpoint by hand.

## Confirmed on a live controller (OS 3.3)

- SDDP discovery binds the driver, and Director's driver connection on 6100 works.
- `addcli` → `session` → RTP to `HOST:PORT` → audio out, using a stream created
  with `addstr`/`addmed`.
- The reply format, the quoted session argument, the `sclock` format, and master
  clock sync.
- Real-time, synchronised playback at the right pitch (48 kHz, Control4's
  `quality 48K/24`) once the timestamp extension above is stripped.

## Still open

1. Director does not yet add this endpoint to a stream by itself. With a room's
   endpoint set to the demo driver, Digital Media created a stream but never
   loaded a source or subscribed the endpoint.
2. Whether the per-stream clock (`sclock`) answers in some other mode. Playing on the
   master clock with the session's `btime` works.
