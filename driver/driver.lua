-- Audio Client (Open): demo driver for c4-audio-client.
--
-- Director routes Digital Media to the "Digital Audio Client" input and plays it to
-- the box itself; this driver only carries room volume/mute to the client over its
-- control port (6100, the same line protocol Control4's streamer uses) and reports
-- what it sees in the Properties so a first test on a real controller is readable.

local NET, PORT, PROXY = 6001, 6100, 5001
local FIRST_OUTPUT, FIRST_ZONE = 4000, 6200 -- Audio Output N plays client zone 6200+N
local OUTPUTS = 4

local tag, pending, buffer = 0, {}, ""
local volume, muted = {}, {}

local function prop(name, value) C4:UpdateProperty(name, tostring(value)) end
local function dbg(msg) if Properties["Debug"] == "On" then print("[audio client] " .. msg) end end

-- Room-selection bindings (7000+N) and outputs (4000+N) both name output N.
local function outputOf(params)
  local o = tonumber(params and (params.OUTPUT or params.output))
  if not o then return nil end
  if o >= 7000 then o = o - 3000 end
  if o >= FIRST_OUTPUT and o < FIRST_OUTPUT + OUTPUTS then return o end
end

local function send(cmd, output, kind)
  tag = tag + 1
  local t = "d" .. tag
  pending[t] = { output = output, kind = kind }
  local line = t .. " " .. cmd
  prop("Last Command", line)
  dbg("> " .. line)
  C4:SendToNetwork(NET, PORT, line .. "\r\n")
end

local function zoneOf(output) return FIRST_ZONE + (output - FIRST_OUTPUT) end

local function reportVolume(output)
  C4:SendToProxy(PROXY, "VOLUME_LEVEL_CHANGED", { OUTPUT = output, LEVEL = volume[output] or 0 }, "NOTIFY")
end

local function reportMute(output)
  C4:SendToProxy(PROXY, "MUTE_CHANGED", { OUTPUT = output, MUTE = muted[output] and "True" or "False" }, "NOTIFY")
end

local function setVolume(output, level)
  level = math.max(0, math.min(100, math.floor(tonumber(level) or 0)))
  volume[output] = level
  send(string.format("setvol %d %d %d", zoneOf(output), level, level), output)
  reportVolume(output)
end

local function setMute(output, on)
  muted[output] = on
  send(string.format("setmut %d %s", zoneOf(output), on and "ON" or "OFF"), output)
  reportMute(output)
end

local function refresh()
  for i = 0, OUTPUTS - 1 do
    local o = FIRST_OUTPUT + i
    send("getvol " .. zoneOf(o), o, "vol")
    send("getmut " .. zoneOf(o), o, "mut")
  end
end

-- Replies (Control4's convention): data lines "<tag> <cmd> <data>", then the status
-- line "<tag> OK <cmd>", "<tag> NO <cmd>" or "<tag> BAD <cmd> <msg>".
local function onLine(line)
  prop("Last Reply", line)
  dbg("< " .. line)
  local t, word, rest = line:match("^(%S+)%s+(%S+)%s*(.*)$")
  local req = t and pending[t]
  if not req then return end
  if word == "OK" or word == "NO" or word == "BAD" then
    pending[t] = nil
  elseif req.kind == "vol" and tonumber(rest) then
    volume[req.output] = tonumber(rest); reportVolume(req.output)
  elseif req.kind == "mut" then
    muted[req.output] = (rest == "ON"); reportMute(req.output)
  end
end

function ReceivedFromNetwork(idBinding, nPort, strData)
  buffer = buffer .. strData
  for line in buffer:gmatch("([^\r\n]*)\r?\n") do
    if line ~= "" then onLine(line) end
  end
  buffer = buffer:match("[^\n]*$") or ""
end

function OnConnectionStatusChanged(idBinding, nPort, strStatus)
  if idBinding ~= NET then return end
  local addr = C4:GetBindingAddress(NET) or "?"
  prop("Status", strStatus .. " (" .. addr .. ":" .. PORT .. ")")
  if strStatus == "ONLINE" then
    send("vers")
    refresh()
  end
end

function OnNetworkBindingChanged(idBinding, bIsBound)
  if idBinding ~= NET then return end
  prop("Status", bIsBound and "bound, connecting" or "not bound")
  if bIsBound then C4:NetConnect(NET, PORT) end
end

function ReceivedFromProxy(idBinding, strCommand, tParams)
  prop("Last Proxy Command", strCommand .. " " .. C4:JsonEncode(tParams or {}))
  dbg("proxy " .. strCommand)
  local o = outputOf(tParams)

  if strCommand == "SET_INPUT" then
    -- One input: every output plays the Digital Audio Client. Acknowledge the route.
    C4:SendToProxy(PROXY, "INPUT_OUTPUT_CHANGED", { INPUT = tParams.INPUT, OUTPUT = tParams.OUTPUT }, "NOTIFY")
  elseif not o then
    return
  elseif strCommand == "SET_VOLUME_LEVEL" then
    setVolume(o, tParams.LEVEL)
  elseif strCommand == "PULSE_VOL_UP" then
    setVolume(o, (volume[o] or 0) + 2)
  elseif strCommand == "PULSE_VOL_DOWN" then
    setVolume(o, (volume[o] or 0) - 2)
  elseif strCommand == "START_VOL_UP" then
    send("ramp " .. zoneOf(o) .. " UP", o)
  elseif strCommand == "START_VOL_DOWN" then
    send("ramp " .. zoneOf(o) .. " DOWN", o)
  elseif strCommand == "STOP_VOL_UP" or strCommand == "STOP_VOL_DOWN" then
    send("ramp " .. zoneOf(o) .. " OFF", o)
    send("getvol " .. zoneOf(o), o, "vol")
  elseif strCommand == "MUTE_ON" then
    setMute(o, true)
  elseif strCommand == "MUTE_OFF" then
    setMute(o, false)
  elseif strCommand == "MUTE_TOGGLE" then
    setMute(o, not muted[o])
  end
end

function OnDriverLateInit()
  prop("Status", "connecting")
  C4:NetConnect(NET, PORT)
end
