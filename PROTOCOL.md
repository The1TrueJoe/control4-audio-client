# Control4 OS3 "audio3" streaming protocol

Reverse-engineered from a Control4 EA-3's factory root filesystem (OS 3.3.0.628678):
`/control4/bin/audio3{client,server,streamer,clock}` (i586, stripped, C++/boost/
GStreamer 1.10.3).

No Control4 code is reproduced here, only names, constants and message shapes.

## Pieces

| Process | Runs on | Role | Port |
|---|---|---|---|
| `control4_digitalaudio.c4l` | director | decides what plays where | — |
| `audio3server` | director host | stream manager, spawns one streamer per stream | TCP **6000** |
| `audio3streamer` | director host | GStreamer source -> FLAC -> RTP; connects *to* each client | per-stream RTCP + clock ports from a pool |
| `audio3clock` | director host | master `GstNetTimeProvider` | UDP **6002** (`NETCLOCK_PORT`) |
| `audio3client` | every endpoint | receives RTP, plays through ALSA | TCP **6100** |

Ports come from the device `.c4i` (`opt/control4/var/drivers/c4i/control4_ea3.c4i`),
class `TCP`: `Audioserver 6000`, `Audioclient 6100`, both CRLF (`<delimiter>0d0a`).
**Plain TCP, no TLS.**

Flow:

```
digitalaudio --addstr/addmed/addcli/setsta--> audio3server:6000
audio3server --spawns--> audio3streamer --stream --master --rtcp --clock --mcast
audio3streamer --TCP--> client:6100   "<tag> session session_cmd, cmd=0, ..."
audio3streamer --RTP/UDP (FLAC in rtpgstpay)--> client
client --RTCP--> streamer:<rtcp>
client --GstNetClientClock--> sclock (stream clock) / director:6002 (master)
```

`addcli` args (streamer side, `AudioStreamer::cmdAddCli`): `STREAM_INDEX CLIENT PORT`:
stream index, the client address, and the client's output port (strtol).

## Control channel (client listens on TCP 6100)

IMAP-style, line-based, CRLF terminated:

```
-> <tag> <command> [args...]\r\n          args split on ' ' (PARAM_DELIMETER = 0x20)
<- <tag> OK [data]\r\n                   success  (fmt "{} {} {}" / "{} {}", OK|NO)
<- <tag> NO <message>\r\n                handled, but failed
<- <tag> BAD <command> <message>\r\n      failure  (fmt "{} BAD {} {}")
<- * BAD Missing command or tag.\r\n      line with < 2 tokens
<- * SYS <EVENT>=...\r\n                  unsolicited event (e.g. "* SYS ADVANCMNT=")
```

Commands are lowercased before lookup. The streamer sends `PING` as a keepalive
(10 s timeout); it expects a reply. Error strings it understands:
`Unsupported command.`, `Wrong number of arguments.`.

### Client command table (from `audio3client` registration)

| cmd | args | notes |
|---|---|---|
| `session` | `CMD` (one serialized GstStructure) | **the important one**, see below |
| `addch` | `PORT` | add an output channel (port = output id, 6200..6206) |
| `remch` | `PORT` | |
| `setvol` | `PORT LEFT RIGHT` | 0-100; L≠R is averaged |
| `getvol` | `PORT` | |
| `setmut` | `PORT STATE` | ON/OFF |
| `getmut` / `togmut` | `PORT` | |
| `setmaxvol` / `getmaxvol` | `PORT VALUE` / `PORT` | |
| `ramp` | `PORT DIR` | UP/DOWN/OFF |
| `getparam` / `setparam` | `[PORT]:KEY [VALUE]` | keys: `latency`, `latency_profile` (default/higher/highest/custom), … |
| `listparams`, `status`, `vers`, `reset`, `initialize` | | |
| `prepadvancmnt` / `playadvancmnt` / `finadvancmnt` | `FILE_URL DATA MAX_AUDIO_SECONDS FILE_TIME_STAMP` / `START_TIME` | announcements; replies with `* SYS ADVANCMNT=ADV_PREP_COMPLETED` etc. |
| `alsasetvol` / `alsarampvol` / `alsaenablevol` | | ALSA-element volume |
| `getdisablehdmiaudio` / `setdisablehdmiaudio` | `STATE` | |

For a minimal client: `session`, `PING`, `addch`/`remch`, `setvol`/`getvol`,
`setmut`/`getmut`/`togmut`, `status`, `vers`. Reply OK to the rest.

### `session` payload

The argument is `gst_structure_to_string()` output; parse it with `gst_structure_from_string`
(or a small parser). Built by the streamer (`Session::...`, `session.cpp`):

```
session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)<streamer rtcp port>,
             btime=(guint64)<pipeline base time ns>, sclock=(string)"<host>:<port>"
             [, mcast=(string)"<group>:<port>"]
```

| `cmd` | meaning | fields |
|---|---|---|
| 0 | PLAY stream | port, rtcp, btime, sclock, [mcast] |
| 2 | STOP stream | port |
| 4 | PLAY announcement | same as 0 |
| 5 | STOP announcement | port |
| 1, 3 | unused by the streamer (client accepts them as non-PLAY) | |

Client behaviour (`AudioClient::handleSessionCmd`): a PLAY for a port that already
has an active PLAY tears the old one down first. Success reply carries a
`session_reply` structure (`gst_structure_new_empty("session_reply")` + fields, serialized).
Failure strings: `Unable to parse session info.`, `Session command failed.`,
`Invalid configuration`.

## Media

- **Codec:** FLAC (`flacenc` -> `audio/x-flac, framed=true`), stereo, 44.1/48/96/192 kHz,
  16/24 bit (quality setting; default 48000, `C4_DEFAULT_QUALITY`=1).
- **Payload:** GStreamer's own `rtpgstpay`, PT 96, `encoding-name=X-GST`, clock-rate 90000.
  Caps travel in-band (base64 in the `caps=` field of the RTP caps), e.g.
  `audio/x-flac, channels=2, framed=true, rate=44100, layout=interleaved, channel-mask=0x3`.
  Receiver: `rtpgstdepay ! flacdec`.
- **RTP session:** `rtpbin` with `rtp-profile=avpf`, `do-retransmission=true` (RTX via
  `rtprtxreceive`, PT map `application/x-rtp-pt-map`), `buffer-mode=synced`,
  `latency` = 1200 ms by default (300..3000). RTCP to the streamer's `rtcp` port.
- **Multicast:** optional `mcast` group; client funnels unicast and multicast RTP into one
  `rtpbin` input.
- **Announcements:** port + `ANNOUNCEMENT_PORT_OFFSET` (100).

## Now-playing metadata

`audio3streamer` turns source metadata (title/artist/album, `c4audio` "expedited
metadata") into GStreamer tag events and sends them into its distribution pipeline.
`rtpgstpay` serializes tag events into the RTP stream, so they reach the client
in-band: `rtpgstdepay` re-emits them and they surface as `GST_MESSAGE_TAG` on the
bus. The stock client ignores them. Cover art and the director's own now-playing
(service, queue) are not in the stream; those come from the director's API.

## Clock / sync

Plain GStreamer network-synchronised playback:

- Clock protocol is stock `GstNetTimeProvider`: 16-byte UDP packets, two big-endian
  u64 nanosecond timestamps (local send time, remote time). Easy to implement natively.
- Master clock: `audio3clock --mport 6002` on the director host (rate-smoothed, see
  `/etc/clock_sync.conf`). Per-stream clock: `sclock` from the session.
- Client: `pipeline.use_clock(GstNetClientClock(sclock))`, `set_base_time(btime)`,
  `set_start_time(NONE)`, then PLAYING once the clock reports synced.
- `/etc/audio_sync.conf` holds per-model/per-port/rate/bit-depth render offsets (ns), e.g.
  `ea3,6200,48000,24,8,113750`. Our client needs its own equivalent knob.

## How the director picks a client (open question, biggest risk)

A client is a project device whose driver declares:

- a `TCP` network class with a port named `Audioclient`, number `6100`, and
- a consumer connection of class `DIGITAL_AUDIO_CLIENT` (EA3: id 3003, autobind), which
  binds to the Digital Media driver's `DIGITAL_AUDIO_CLIENT` (id 4000).

`libc4libdriverbases.so` (`C4BaseController::SetAudioClientState`,
`SetAudioClientFromBinding`, `OnConnectionStatusChanged_AudioClient`) is how Control4's
own controller drivers turn the local `audio3client` on/off via sysman when a binding
exists. Still unverified: whether a third-party driver (DriverWorks `.c4z`) that
declares the same connection + port is enough for `digitalaudio` to `addcli` it.
If not, the fallback is to present as an existing Control4 endpoint type.

## Still to confirm on the wire (capture on a stock controller)

Capture `tcp port 6100 or tcp port 6000 or udp` while playing a stream to that room:

1. Exact tag format the streamer uses, and what it reads from `session_reply`.
2. Which UDP port the client receives RTP on (the `port` value itself, or a derived one)
   and where its RTCP-in lives. The decompile drops these arguments.
3. `sclock` string format (expected `host:port`).
4. Full `addcli` line from digitalaudio to `audio3server`.
