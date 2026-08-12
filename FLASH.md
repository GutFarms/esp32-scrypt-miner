# Flash the ESP32-2432S028 (CYD) scrypt miner

Chip: **ESP32** (WROOM-32) · Board: **ESP32-2432S028** · Image: `esp,lite`

## Board flash profile

| Setting | Value |
|---------|-------|
| Crystal | **40 MHz** |
| Flash size | **4 MB** |
| Flash mode | **DIO** |
| Flash SPI clock | **40 MHz** |
| Example chip id | `0x57CAFC` (board-specific) |

Merged images are built with `--flash-mode dio --flash-freq 40mhz --flash-size 4mb` so the ESP image header matches this CYD profile.

## Save `.bin` to your PC (recommended)

```bash
./scripts/build-flash-images.sh
./scripts/serve-web-flasher.sh
```

Open **http://127.0.0.1:8080/web/** → **Save merged.bin to PC**.

Downloads as:

`esp32-2432s028-scrypt-miner-merged.bin` — flash at **`0x0`** (~1 MiB, bootloader + partitions + app)

Verify (optional):

```bash
cd flash && sha256sum -c SHA256SUMS.txt
```

Or copy without the browser:

```bash
cp flash/esp32-2432s028-scrypt-miner-merged.bin ~/Downloads/
# Windows (Git Bash): cp flash/esp32-2432s028-scrypt-miner-merged.bin "$USERPROFILE/Downloads/"
```

## Flash from the browser

Chrome / Edge only:

1. http://127.0.0.1:8080/web/ (page auto-loads the project image when available)
2. **Save merged.bin to PC** if you want a local copy
3. **Connect & flash** → select the COM / tty port  
   Hold **BOOT** while tapping **RESET** if connect stalls
4. Wait for “Flash complete”

One-click alternate: http://127.0.0.1:8080/web/install.html

## Flash from the CLI

```bash
# Windows
espflash write-bin -p COM6 0x0 flash/esp32-2432s028-scrypt-miner-merged.bin
espflash monitor -p COM6

# Linux / macOS
espflash write-bin -p /dev/ttyUSB0 0x0 flash/esp32-2432s028-scrypt-miner-merged.bin
espflash monitor -p /dev/ttyUSB0

# Or ELF path (auto bootloader)
./scripts/flash-cyd.sh COM6
```

Install **CH340** drivers on Windows if no COM port appears. Serial monitor: **115200**.

## Image files

| File | Address | Notes |
|------|---------|-------|
| `flash/esp32-2432s028-scrypt-miner-merged.bin` | `0x0` | **Use this** — padded only to end of app (~1 MiB) |
| `flash/esp32-2432s028-scrypt-miner.bin` | `0x10000` | App only |
| `flash/SHA256SUMS.txt` | — | Checksums from last build |

## First boot

**Recommended — CYD Companion (USB only):**

1. Board shows **Waiting for companion** (no touch/PuTTY typing)
2. Run `cyd-companion.exe` → USB → Connect → **Setup** → WiFi + Pool → **Save & reboot**
3. After reboot + DHCP: mining + `http://<board-ip>/`

**Fallback — on-device setup:** hold **BOOT** at power-on, then use touch keyboard or serial (WiFi → stratum → worker → password).

## LCD & WiFi

- LCD **stays on** (no sleep / backlight-off timer).
- WiFi STA stays **always on** (modem power-save disabled; auto-reconnect on drop).
- If DHCP/IP is missing for **>10 minutes**, the board soft-resets (NMMiner-style radio recovery).
- Hold **BOOT** ~0.7s for menu / change credentials.
- Touch: default ESPHome CYD map. PuTTY `touch` cycles the axis map (saved). On an empty WiFi scan, short **BOOT** also cycles the map.

## HTTP API

| Path | Purpose |
|------|---------|
| `/` | Status dashboard (HTML) |
| `/api/status` | Compact JSON status |
| `/probe` | Discovery JSON (`hr` + `ver` for LAN monitors) |
| `/alive` | Liveness / self-IP JSON |
| `/api/system/info` | Fuller JSON (hashrate, shares, pool, WiFi, uptime, LCD) |
| `/api/reconnect` | Request stratum reconnect |

## Not for

- ESP32-S2 / C3 / S3 modules (wrong chip image)
- JCHC-1 ASIC controller (hardware-only package)
