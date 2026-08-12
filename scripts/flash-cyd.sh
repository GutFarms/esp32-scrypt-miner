#!/usr/bin/env bash
# Flash ESP32-2432S028 (CYD) scrypt miner.
# Usage: ./scripts/flash-cyd.sh [PORT]
#   PORT examples: COM6, /dev/ttyUSB0, /dev/ttyACM0
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PORT="${1:-}"
for f in "$ROOT/export-esp.sh" "$HOME/export-esp.sh" "$ROOT/export-esp.sh.example"; do
  if [[ -f "$f" ]]; then
    # shellcheck disable=SC1090
    source "$f"
    break
  fi
done

ELF="$ROOT/target/xtensa-esp32-none-elf/release/esp32-s3-scrypt-miner"
MERGED="$ROOT/flash/esp32-2432s028-scrypt-miner-merged.bin"

if [[ ! -f "$ELF" ]]; then
  echo "No ELF yet — building..."
  "$ROOT/scripts/build-flash-images.sh"
fi

PORT_ARGS=()
if [[ -n "$PORT" ]]; then
  PORT_ARGS=(-p "$PORT")
fi

echo "==> Flashing ELF to ESP32 (4MB · DIO · 40MHz)..."
espflash flash --monitor --chip esp32 \
  --flash-size 4mb --flash-mode dio --flash-freq 40mhz \
  "${PORT_ARGS[@]}" "$ELF"
