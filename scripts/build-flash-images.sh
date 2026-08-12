#!/usr/bin/env bash
# Build release firmware and write flashable .bin images into ./flash/
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

source_esp_env() {
  local f
  for f in "$ROOT/export-esp.sh" "$HOME/export-esp.sh" "$ROOT/export-esp.sh.example"; do
    if [[ -f "$f" ]]; then
      # shellcheck disable=SC1090
      source "$f"
      return 0
    fi
  done
  echo "warning: no export-esp.sh found; relying on PATH" >&2
}
source_esp_env

mkdir -p flash
echo "==> Building xtensa-esp32-none-elf release (esp,lite)..."
cargo +esp build -Zbuild-std=core,alloc --release \
  --target xtensa-esp32-none-elf --features esp,lite

ELF="$ROOT/target/xtensa-esp32-none-elf/release/esp32-s3-scrypt-miner"
test -f "$ELF"

APP_BIN="flash/esp32-2432s028-scrypt-miner.bin"
MERGED_BIN="flash/esp32-2432s028-scrypt-miner-merged.bin"

# ESP32-2432S028 (CYD / WROOM-32): 40 MHz crystal, 4 MB flash, DIO.
# These flags are written into the ESP image header so ROM boot uses the
# correct SPI mode/clock regardless of the host flasher defaults.
FLASH_ARGS=(--chip esp32 --flash-size 4mb --flash-mode dio --flash-freq 40mhz)

echo "==> Writing app image (flash @ 0x10000, DIO @ 40 MHz, 4 MB)..."
espflash save-image "${FLASH_ARGS[@]}" \
  "$ELF" "$APP_BIN"

echo "==> Writing merged image (flash @ 0x0, no 4MiB pad)..."
# --skip-padding keeps the download ~1 MiB instead of a full 4 MiB sparse image.
espflash save-image "${FLASH_ARGS[@]}" --merge --skip-padding \
  "$ELF" "$MERGED_BIN"

# Basic sanity: ESP app magic 0xE9 at start of app image and at 0x10000 in merged.
python3 - <<'PY'
from pathlib import Path
import sys
app = Path("flash/esp32-2432s028-scrypt-miner.bin").read_bytes()
merged = Path("flash/esp32-2432s028-scrypt-miner-merged.bin").read_bytes()
ok = True
if app[0] != 0xE9:
    print("ERROR: app.bin missing ESP magic 0xE9", file=sys.stderr); ok = False
if len(merged) < 0x10000 + 256:
    print("ERROR: merged.bin too small", file=sys.stderr); ok = False
elif merged[0x10000] != 0xE9:
    print("ERROR: merged.bin has no app at 0x10000", file=sys.stderr); ok = False
elif merged[0x1000] != 0xE9:
    print("ERROR: merged.bin has no bootloader at 0x1000", file=sys.stderr); ok = False
if app[:16] != merged[0x10000:0x10010]:
    print("ERROR: merged app segment != app.bin", file=sys.stderr); ok = False
# Image header: byte2 flash mode (2=DIO), byte3 size/freq (4MB + 40MHz → 0x20)
mode = app[2]
size_freq = app[3]
if mode != 0x02:
    print(f"ERROR: flash mode byte={mode:#04x}, expected DIO (0x02)", file=sys.stderr); ok = False
if (size_freq & 0x0F) != 0x0:
    print(f"ERROR: flash freq nibble={size_freq & 0x0F:#x}, expected 40MHz (0x0)", file=sys.stderr); ok = False
if (size_freq >> 4) != 0x2:
    print(f"ERROR: flash size nibble={size_freq >> 4:#x}, expected 4MB (0x2)", file=sys.stderr); ok = False
print(f"Header OK: magic=0xE9 mode=DIO size/freq={size_freq:#04x} (4MB @ 40MHz)")
sys.exit(0 if ok else 1)
PY

echo "==> Writing SHA256SUMS.txt..."
(
  cd flash
  sha256sum esp32-2432s028-scrypt-miner.bin esp32-2432s028-scrypt-miner-merged.bin > SHA256SUMS.txt
  cat SHA256SUMS.txt
)

ls -la "$APP_BIN" "$MERGED_BIN"
echo "Done. Save/flash via: ./scripts/serve-web-flasher.sh  →  http://127.0.0.1:8080/web/"
