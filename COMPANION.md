# CYD Companion (Windows)

GPU desktop app (`cyd-companion.exe`) for the ESP32-2432S028 scrypt miner.

## Download / build

Prebuilt zip: `dist/cyd-companion-windows.zip`

```bash
./scripts/build-companion-windows.sh
```

Or locally on Windows (MSVC/GNU):

```bash
cargo build --no-default-features --features companion --bin cyd-companion --release
```

Release builds on Windows use the `windows` subsystem — **no console/terminal window** on launch. Debug builds still show a console for logs.

## First-time setup (app only)

1. Flash matching firmware (`esp` + `lite`)
2. Plug USB (CH340) — board shows **Waiting for companion**
3. Run `cyd-companion.exe` → **USB** → COM port → **Connect**
4. Open **Setup** → fill WiFi + Pool (+ options) → **Save & reboot**

No touch keyboard or PuTTY typing is required.  
Fallback: hold **BOOT** at power-on for classic on-device setup.

## Features

| Tab | What it does |
|-----|----------------|
| Setup | All-in-one WiFi + pool + CPU + touch map → flash & reboot |
| Dashboard | Live hashrate, pool, WiFi, CPU MHz + 5-coin price strip |
| Markets | Live CoinGecko stats; pick which 5 coins are shown |
| WiFi | SSID + optional **Update WiFi password** |
| Pool | Stratum / worker / pool password |
| Overclock | Max mine default **240 MHz + hash-focus**. Sample H/s at 80/160/240 if needed |
| More | Reconnect, discover, tips |

Look: bubbly blue/black UI with **lime** accents.

## Connection modes

### USB (default)

Plug the CYD USB cable into the PC (same CH340 COM port used for flashing).

1. Select **USB**
2. Pick the COM port → **Refresh** if needed
3. **Connect** (115200 baud)
4. Use **Setup** (first boot) or WiFi / Pool tabs later

USB protocol (device replies with one line):

```text
cmp ping                 → CMP ok usb
cmp status               → CMPSTATUS {…}
cmp config               → CMPCONFIG {…,"configured":true|false}
cmp set auth=…&wifi_ssid=…&wifi_password=…&worker=…&stratum=…&password=…&cpu_mhz=160
cmp clock auth=…&cpu_mhz=240
cmp reboot auth=…
```

Unconfigured boards accept `cmp set` **without** pool auth. After save, Auth = pool password.

### LAN (HTTP)

When the miner is already on Wi‑Fi:

1. Select **LAN**
2. Enter `http://<miner-ip>/` (or just the IP)
3. Enter pool password → **Connect**

## Board APIs used (LAN)

- `GET /api/status`, `GET /api/config`, `GET /probe`
- `POST /api/config`, `POST /api/clock`, `POST /api/reboot`, `POST /api/reconnect`
