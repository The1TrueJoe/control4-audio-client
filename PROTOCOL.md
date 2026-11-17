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
<- <tag> OK [data]\r\n                 success
<- <tag> NO <message>\r\n              understood, but failed
<- <tag> BAD <command> <message>\r\n   unknown command or wrong arguments
<- * BAD <message>\r\n                 line without a tag and a command
<- * SYS <EVENT>=...\r\n               unsolicited event
```

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

The argument is a serialized GstStructure (`gst_structure_from_string` parses it):

```
session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)<streamer rtcp port>,
             btime=(guint64)<pipeline base time, ns>, sclock=(string)"<host>:<port>"
             [, mcast=(string)"<group>:<port>"]
```

| `cmd` | meaning | fields |
|---|---|---|
| 0 | play stream | port, rtcp, btime, sclock, [mcast] |
| 2 | stop stream | port |
| 4 | play announcement | as 0 |
| 5 | stop announcement | port |

A play for a port that is already playing replaces the old stream. The endpoint
answers `<tag> OK session_reply;` (a serialized, possibly empty, `session_reply`
structure).

## Media

- **Codec:** FLAC, stereo, 44.1–192 kHz, 16/24 bit.
- **Payload:** GStreamer's `rtpgstpay` (payload type 96, `encoding-name=X-GST`,
  clock-rate 90000). The FLAC caps travel in-band; receive with
  `rtpgstdepay ! flacparse ! flacdec`.
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

- The clock protocol is `GstNetTimeProvider`: 16-byte UDP packets carrying two
  big-endian u64 nanosecond timestamps.
- The endpoint sets its pipeline clock to a `GstNetClientClock` on `sclock` (or
  Director's host :6002 when `sclock` is absent), sets the base time to `btime` and
  the start time to none, and plays once the clock is synced.
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

## Still to confirm on the wire

1. Which UDP port the endpoint receives RTP on. This client assumes the session's
   `port` itself.
2. What, if anything, the streamer reads from `session_reply`.
3. The exact `sclock` format (assumed `host:port`).
