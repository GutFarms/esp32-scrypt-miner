#!/usr/bin/env bash
# Clean rebuild + verify host tests, firmware image, and Windows companion zip.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

source_esp_env() {
  local f
  for f in "$ROOT/export-esp.sh" "$HOME/export-esp.sh"; do
    if [[ -f "$f" ]]; then
      # shellcheck disable=SC1090
      source "$f"
      return 0
    fi
  done
  echo "warning: no export-esp.sh — firmware build may fail" >&2
}

echo "==> cargo clean"
cargo clean

echo "==> host unit tests (full-memory N=1024)"
cargo test --no-default-features --lib

echo "==> host unit tests (lite TMTO N=1024)"
cargo test --no-default-features --features lite --lib

echo "==> stratum + config tests already covered above; building host-miner"
cargo build --no-default-features --features host --bin host-miner --release

echo "==> firmware + flash images"
source_esp_env
./scripts/build-flash-images.sh

echo "==> Windows companion"
./scripts/build-companion-windows.sh

APP_BIN="flash/esp32-2432s028-scrypt-miner.bin"
MERGED_BIN="flash/esp32-2432s028-scrypt-miner-merged.bin"
ZIP="dist/cyd-companion-windows.zip"
PORTABLE_ZIP="dist/CYD-Companion-Portable.zip"
APP_ONLY_ZIP="dist/CYD-Companion-App-Only.zip"
EXE="dist/cyd-companion-windows/cyd-companion.exe"
SETUP="dist/CYD-Companion-Setup.exe"

echo "==> verifying artifacts"
test -f "$APP_BIN"
test -f "$MERGED_BIN"
test -f "$ZIP"
test -f "$PORTABLE_ZIP"
test -f "$APP_ONLY_ZIP"
test -f "$EXE"
test -f "$SETUP"

python3 - <<'PY'
from pathlib import Path
import sys
import zipfile

app = Path("flash/esp32-2432s028-scrypt-miner.bin").read_bytes()
merged = Path("flash/esp32-2432s028-scrypt-miner-merged.bin").read_bytes()
ok = True
if app[0] != 0xE9:
    print("ERROR: app.bin missing ESP magic 0xE9", file=sys.stderr); ok = False
if len(merged) < 0x10000 + 256:
    print("ERROR: merged.bin too small", file=sys.stderr); ok = False
elif merged[0x1000] != 0xE9:
    print("ERROR: merged.bin missing bootloader magic @ 0x1000", file=sys.stderr); ok = False
elif merged[0x10000] != 0xE9:
    print("ERROR: merged.bin missing app magic @ 0x10000", file=sys.stderr); ok = False
elif app[:16] != merged[0x10000:0x10010]:
    print("ERROR: merged app segment != app.bin", file=sys.stderr); ok = False

zpath = Path("dist/cyd-companion-windows.zip")
with zipfile.ZipFile(zpath) as zf:
    names = set(zf.namelist())
    need = {
        "cyd-companion-windows/cyd-companion.exe",
        "cyd-companion-windows/COMPANION.md",
        "cyd-companion-windows/README.txt",
    }
    missing = sorted(need - names)
    if missing:
        print("ERROR: zip missing:", ", ".join(missing), file=sys.stderr)
        ok = False
    info = zf.getinfo("cyd-companion-windows/cyd-companion.exe")
    if info.file_size < 1_000_000:
        print("ERROR: companion exe unexpectedly small", file=sys.stderr)
        ok = False

sums = Path("flash/SHA256SUMS.txt").read_text().splitlines()
if len(sums) < 2:
    print("ERROR: SHA256SUMS.txt incomplete", file=sys.stderr)
    ok = False

sys.exit(0 if ok else 1)
PY

echo "==> copying downloadable artifacts"
mkdir -p /opt/cursor/artifacts
cp -f "$MERGED_BIN" /opt/cursor/artifacts/
cp -f "$ZIP" /opt/cursor/artifacts/
cp -f "$PORTABLE_ZIP" /opt/cursor/artifacts/
cp -f "$SETUP" /opt/cursor/artifacts/
cp -f flash/SHA256SUMS.txt /opt/cursor/artifacts/esp32-2432s028-SHA256SUMS.txt

echo
echo "OK — verified clean build"
ls -la "$MERGED_BIN" "$ZIP" "$PORTABLE_ZIP" "$SETUP" \
  /opt/cursor/artifacts/esp32-2432s028-scrypt-miner-merged.bin \
  /opt/cursor/artifacts/cyd-companion-windows.zip \
  /opt/cursor/artifacts/CYD-Companion-Portable.zip \
  /opt/cursor/artifacts/CYD-Companion-Setup.exe
echo
echo "SHA256:"
sha256sum "$MERGED_BIN" "$ZIP" "$PORTABLE_ZIP" "$SETUP"
cat flash/SHA256SUMS.txt
