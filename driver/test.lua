-- Self-check for driver.lua against a stubbed C4 API: cd driver && lua test.lua
local sent, notified, props = {}, {}, {}
C4 = {
  UpdateProperty = function(_, k, v) props[k] = v end,
  SendToNetwork = function(_, id, port, data) sent[#sent + 1] = data end,
  SendToProxy = function(_, id, cmd, params) notified[#notified + 1] = { cmd = cmd, params = params } end,
  NetConnect = function() end,
  GetBindingAddress = function() return "10.0.0.9" end,
  JsonEncode = function() return "{}" end,
}
Properties = { Debug = "Off" }
dofile("driver.lua")

ReceivedFromProxy(5001, "SET_VOLUME_LEVEL", { OUTPUT = "4001", LEVEL = "35" })
assert(sent[#sent] == "d1 setvol 6201 35 35\r\n", sent[#sent])
assert(notified[#notified].cmd == "VOLUME_LEVEL_CHANGED" and notified[#notified].params.LEVEL == 35)

ReceivedFromProxy(5001, "MUTE_TOGGLE", { OUTPUT = "7003" }) -- room selection 4 -> output 4003
assert(sent[#sent] == "d2 setmut 6203 ON\r\n", sent[#sent])

ReceivedFromProxy(5001, "SET_VOLUME_LEVEL", { OUTPUT = "4000", LEVEL = "250" })
assert(sent[#sent] == "d3 setvol 6200 100 100\r\n", "volume clamps to 100")

-- getvol reply, split across two network reads, reports back to the proxy.
OnConnectionStatusChanged(6001, 6100, "ONLINE") -- sends vers (d4) then getvol/getmut per output (d5..)
assert(props.Status == "ONLINE (10.0.0.9:6100)")
ReceivedFromNetwork(6001, 6100, "d5 OK 4")
ReceivedFromNetwork(6001, 6100, "2\r\nd6 OK ON\r\n")
local n = #notified
assert(notified[n - 1].cmd == "VOLUME_LEVEL_CHANGED" and notified[n - 1].params.LEVEL == 42)
assert(notified[n].cmd == "MUTE_CHANGED" and notified[n].params.MUTE == "True")
print("driver selfcheck OK")
