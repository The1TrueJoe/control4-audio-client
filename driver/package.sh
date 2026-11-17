#!/usr/bin/env bash
# Builds the demo DriverWorks driver: a .c4z is a plain zip of driver.xml + driver.lua.
set -euo pipefail
cd "$(dirname "$0")"
out="$(pwd)/c4-audio-client.c4z"
rm -f "$out"
zip -X -q "$out" driver.xml driver.lua
echo "built $out"
