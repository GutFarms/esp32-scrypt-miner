#!/usr/bin/env bash
# Serve the drag-and-drop web flasher (needs HTTP for Web Serial + .bin download).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -f flash/esp32-2432s028-scrypt-miner-merged.bin ]]; then
  echo "No merged.bin yet — building flash images..."
  "$ROOT/scripts/build-flash-images.sh"
fi

# Keep companion app zips next to the flasher page for one-click download.
mkdir -p flash/downloads
for f in CYD-Companion-App-Only.zip CYD-Companion-Portable.zip CYD-Companion-Setup.exe; do
  if [[ -f "dist/$f" ]]; then
    cp -f "dist/$f" "flash/downloads/$f"
  elif [[ -f "/opt/cursor/artifacts/$f" ]]; then
    cp -f "/opt/cursor/artifacts/$f" "flash/downloads/$f"
  fi
done

PORT="${1:-8080}"
SIZE="$(wc -c < flash/esp32-2432s028-scrypt-miner-merged.bin | tr -d ' ')"
# LAN IP hints (best-effort) so phones/PCs can open the flasher by IP.
LAN_HINT="$(hostname -I 2>/dev/null | awk '{print $1}')"
echo ""
echo "  CYD web flasher + app downloads"
echo "  -------------------------------"
echo "  Open in Chrome or Edge:"
echo "    http://127.0.0.1:${PORT}/web/"
if [[ -n "${LAN_HINT}" ]]; then
  echo "    http://${LAN_HINT}:${PORT}/web/"
fi
echo ""
echo "  Firmware: Save merged.bin → Connect & flash @ 0x0"
echo "  Windows app: Download Windows app (zip) on the same page"
echo ""
echo "  Direct app zip:"
echo "    http://127.0.0.1:${PORT}/downloads/CYD-Companion-App-Only.zip"
echo ""
echo "  merged.bin size: ${SIZE} bytes"
if [[ -f flash/SHA256SUMS.txt ]]; then
  echo "  checksums: flash/SHA256SUMS.txt"
fi
echo ""
cd flash
exec python3 -m http.server "$PORT" --bind 0.0.0.0
