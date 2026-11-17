"""End-to-end check: plays the part of Control4's streamer against a running client.

Serves a GstNetTimeProvider (the stream clock), sends FLAC in rtpgstpay with tags,
drives the control channel, and checks the zone through the HTTP API.

    python3 test/selfcheck.py target/release/c4-audio-client
"""
import json, socket, subprocess, sys, tempfile, time, urllib.request
import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstNet", "1.0")
from gi.repository import Gst, GstNet

Gst.init(None)
CONFIG = """
control = "127.0.0.1:6100"
api = "127.0.0.1:6180"
[[zone]]
port = 6200
name = "Test"
sink = "fakesink sync=true"
"""

cfg = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
cfg.write(CONFIG)
cfg.close()
client = subprocess.Popen([sys.argv[1], cfg.name])
try:
    clock = Gst.SystemClock.obtain()
    provider = GstNet.NetTimeProvider.new(clock, "127.0.0.1", 16002)
    tx = Gst.parse_launch(
        'audiotestsrc is-live=true ! taginject tags="title=\\"Test Tone\\",artist=Tester" '
        "! audioconvert ! flacenc ! rtpgstpay config-interval=1 ! udpsink host=127.0.0.1 port=6200"
    )
    tx.use_clock(clock)
    base = clock.get_time()
    tx.set_base_time(base)
    tx.set_start_time(Gst.CLOCK_TIME_NONE)
    tx.set_state(Gst.State.PLAYING)

    for _ in range(50):
        try:
            sock = socket.create_connection(("127.0.0.1", 6100))
            break
        except OSError:
            time.sleep(0.1)
    f = sock.makefile("rw", newline="")

    def cmd(line):
        f.write(line + "\r\n")
        f.flush()
        return f.readline().rstrip("\r\n")

    def zone():
        return json.load(urllib.request.urlopen("http://127.0.0.1:6180/api/zones/6200"))

    r = cmd(
        f"a1 session session_cmd, cmd=(int)0, port=(int)6200, rtcp=(int)16201, "
        f'btime=(guint64){base}, sclock=(string)"127.0.0.1:16002"'
    )
    assert r == "a1 session session_reply;" and f.readline().rstrip("\r\n") == "a1 OK session", r
    for _ in range(50):
        z = zone()
        if z["now_playing"].get("title"):
            break
        time.sleep(0.2)
    assert z["playing"] and z["now_playing"].get("title") == "Test Tone", z
    assert z["now_playing"].get("artist") == "Tester", z

    assert cmd("a2 setvol 6200 40 40") == "a2 OK setvol" and zone()["volume"] == 40
    assert cmd("a3 getvol 6200") == "a3 getvol 40" and f.readline().rstrip("\r\n") == "a3 OK getvol"
    assert cmd("a4 setmut 6200 ON") == "a4 OK setmut" and cmd("a5 getmut 6200") == "a5 getmut ON" and f.readline().rstrip("\r\n") == "a5 OK getmut"
    assert cmd("a6 setvol 6299 10 10") == "a6 setvol no such zone" and f.readline().rstrip("\r\n") == "a6 NO setvol"
    assert cmd("a7 bogus") == "a7 BAD bogus unknown command"
    assert cmd("lonely") == "* BAD expected: <tag> <command> [args]"

    req = urllib.request.Request(
        "http://127.0.0.1:6180/api/zones/6200", data=b'{"volume": 70, "muted": false}',
        method="PATCH", headers={"Content-Type": "application/json"},
    )
    v = json.load(urllib.request.urlopen(req))
    assert v["volume"] == 70 and not v["muted"], v

    assert cmd("a8 session session_cmd, cmd=(int)2, port=(int)6200") == "a8 session session_reply;" and f.readline().rstrip("\r\n") == "a8 OK session"
    assert not zone()["playing"]
    print("selfcheck OK")
finally:
    client.terminate()
