# ESP32-2432S028 Scrypt Miner (Cheap Yellow Display)

Bare-metal Rust firmware that mines **scrypt** proof-of-work on an **ESP32-2432S028** (CYD) and shows a multi-screen **GUI** on the onboard **ILI9341** TFT. Uses onboard **WiFi** (STA + DHCP) for stratum and a LAN web UI.

## What it does

- Mines **real Litecoin scrypt** (`N=1024`, `r=1`, `p=1`) — pool-valid hashes
- On this board, **`lite`** uses an 8 KiB checkpointed ROMix (TMTO) so WiFi + stratum still fit in RAM
- Reuses ROMix buffers across hashes
- **After boot**, waits for **CYD Companion** over USB (Setup tab) — optional BOOT-held on-device setup
- Starts **WiFi STA + DHCP** when an SSID is set (BLE unused — RAM kept for WiFi/stratum)
- **Stratum TCP client** over WiFi: subscribe, authorize, receive jobs, submit shares
- **LAN web UI** at `http://<board-ip>/` after DHCP (status dashboard + JSON)
- Discovery APIs (`/probe`, `/alive`, `/api/system/info`); LCD stays on; WiFi always-on (no modem sleep)
- **On-device GUI**: splash, setup, mining dashboard, config tab, radio/pool tab, menu
- Saves credentials to flash and auto-loads them on later boots
- Host CLI (`host-miner`), **desktop GUI** (`host-gui`), and unit tests

Connects and submits real scrypt shares, but hashrate is tiny (MCU + TMTO) — not profitable.

## Relation to NMMiner

[NMMiner](https://github.com/NMminer1024/NMMiner) also targets **ESP32-2432S028**, but mines **Bitcoin SHA-256**. This firmware mines **Litecoin scrypt** (`N=1024`) with a low-memory TMTO on-device.

| | NMMiner | This firmware |
|--|---------|----------------|
| Algorithm | BTC SHA-256 (~1 MH/s) | Scrypt N=1024 (`lite` TMTO) |
| Stack | Arduino + LVGL (~2.7 MiB) | Embassy Rust (~0.85 MiB) |
| Config | SoftAP WiFiManager | UART/PuTTY + LCD scan (WiFi required) |
| LCD sleep | Screensaver prefs | No — LCD stays on |
| WiFi | STA + SoftAP; reboot if down >10 min | STA always-on, no power save; soft-reset if DHCP missing >10 min |
| Touch | Closed `cyd2.8` / `touch_read` HAL | Open XPT2046 SPI3; ESPHome map default; serial `touch` cycles map |
| HTTP | `/probe`, `/alive`, `/api/system/info`, … | Same discovery trio + `/api/status` |

SoftAP captive-portal setup is not used here: classic ESP32 RAM is tight while mining; UART + LCD setup stays the reliable path.

## On-device GUI

| Control | Action |
|---------|--------|
| **Touch** tab strip | Jump to MINE / CONF / RADIO / MENU |
| **Touch** keyboard | Type credentials during setup / auth (OK / skip) |
| **Touch** menu row | Activate option |
| **BOOT** short press | Next tab, or move menu highlight |
| **BOOT** long press (~0.7s) | Open menu / activate selected item |
| **Touch** | Tabs, menu rows, on-screen keyboard |
| Serial `change` | Password-gated credential edit |
| Serial `radio` / `wifi` / `stratum` | Print live radio + pool status |
| Serial `touch` | Cycle XPT2046 axis map (saved to flash) |

Tabs: **MINE** · **CONF** · **RADIO** · **MENU**. LCD stays on. Default touch map is ESPHome CYD (invert X). If taps feel wrong: type `touch` in PuTTY, or short **BOOT** on an empty WiFi scan.

When WiFi is configured, the firmware connects to `stratum` (`host:port` or `stratum+tcp://…`), runs `mining.subscribe` / `mining.authorize`, mines `mining.notify` jobs with the pool difficulty, and submits shares with `mining.submit`. WiFi is required — there is no skip path.

## Post-boot credentials (saved to flash)

On **first boot**, use the **touch screen** or serial monitor (115200):

1. `wifi_ssid` — **required**: scan & tap a network, or **type** the SSID (PuTTY: list number or SSID + Enter)  
2. `wifi_password` — PSK (skipped for open networks)  
3. `stratum` — defaults to `stratum+tcp://ltc.viabtc.io:3333` (Enter keeps it; plain TCP only)  
4. `worker` — worker name (wallet address OK)  
5. `password` — pool password (often `x`)

After WiFi + DHCP, **ONLINE** / pool **CONNECTED** banners appear without pausing mining. Web UI: `http://IP/` · `/api/status` · `/probe` · `/alive` · `/api/system/info` · `/api/reconnect`.

### Change credentials (password required)

- Menu → **Change credentials**, or type `change` on serial  
- Or hold **BOOT** at power-on  

Stratum worker/endpoint/password changes **reconnect without reboot**. After changing WiFi settings, **reboot**.

## Hardware

| Item | Notes |
|------|--------|
| Board | **ESP32-2432S028** (Cheap Yellow Display) |
| MCU | ESP32-WROOM-32 · 40 MHz crystal |
| Flash | **4 MB · DIO · 40 MHz** SPI (baked into image header) |
| Display | ILI9341 SPI, 320×240 landscape, backlight GPIO21 |
| TFT SPI | SCLK=14, MOSI=13, MISO=12, CS=15, DC=2 |
| Buttons | BOOT=GPIO0 (short/long); **touch** tabs + on-screen keyboard |
| Touch | XPT2046 CLK=25 MOSI=32 MISO=39 CS=33 IRQ=36 |
| Serial | USB-UART CH340 → UART0 (TX=1, RX=3) |
| Radio | Onboard WiFi (`esp-radio` + `embassy-net`) |
| Flash config | Sector at `0x3FF000` (end of 4 MiB window) |

## Build & flash (device)

**See [FLASH.md](FLASH.md).**

```bash
. ./export-esp.sh
./scripts/build-flash-images.sh
./scripts/serve-web-flasher.sh
# → http://127.0.0.1:8080/web/  (Chrome / Edge)
#    Save merged.bin to PC  →  Connect & flash
```

CLI: `./scripts/flash-cyd.sh COM6` or  
`espflash write-bin -p COM6 0x0 flash/esp32-2432s028-scrypt-miner-merged.bin`

Hold **BOOT** + **RESET** if connect stalls; install CH340 drivers on Windows if needed.  
Ship with `lite` (TMTO, 8 KiB V). Full-memory `N=1024` (128 KiB V) does not fit with WiFi on this board.

## Host demo, GUI & tests

```bash
# Clean rebuild + tests + firmware .bin + Windows companion zip
./scripts/verify-project.sh

cargo test --no-default-features
cargo run --no-default-features --features host --bin host-miner --release
cargo run --no-default-features --features host-gui --bin host-gui --release
```

## Windows companion app

GPU controller over **USB serial** (default) or LAN — separate **WiFi** / **Pool** tabs + CPU overclock, no console window on release launch. See **[COMPANION.md](COMPANION.md)**.

```bash
./scripts/build-companion-windows.sh
# → dist/cyd-companion-windows.zip  (cyd-companion.exe)
```

Demo difficulty is set by `DEMO_ZERO_NIBBLES` in `src/bin/main.rs` (default `4`).
