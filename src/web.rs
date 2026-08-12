//! Tiny HTTP status + companion control server — `http://<board-ip>/`.
//!
//! Discovery endpoints mirror NMMiner. Companion app uses `/api/config`,
//! `/api/clock`, `/api/reboot`, and `/api/reconnect`.

use heapless::String;

use crate::radio::WifiPhase;
use crate::stratum::StratumPhase;

/// Live snapshot published by the miner loop for the web UI / companion.
#[derive(Clone, Debug)]
pub struct WebStatus {
    pub hashrate_x100: u32,
    pub shares: u64,
    pub nonce: u32,
    pub address: String<96>,
    pub stratum: String<96>,
    pub wifi_ssid: String<32>,
    pub wifi: WifiPhase,
    pub ip: Option<[u8; 4]>,
    pub pool_phase: StratumPhase,
    pub accepted: u32,
    pub rejected: u32,
    pub dropped: u32,
    pub difficulty: u32,
    pub uptime_secs: u64,
    pub screen_on: bool,
    pub cpu_mhz: u8,
    pub hash_focus: bool,
}

impl Default for WebStatus {
    fn default() -> Self {
        Self {
            hashrate_x100: 0,
            shares: 0,
            nonce: 0,
            address: String::new(),
            stratum: String::new(),
            wifi_ssid: String::new(),
            wifi: WifiPhase::Disabled,
            ip: None,
            pool_phase: StratumPhase::Disabled,
            accepted: 0,
            rejected: 0,
            dropped: 0,
            difficulty: 1,
            uptime_secs: 0,
            screen_on: true,
            cpu_mhz: 240,
            hash_focus: true,
        }
    }
}

/// Pending settings change from the Windows companion (applied on the miner loop).
#[derive(Clone, Debug, Default)]
pub struct CompanionUpdate {
    pub auth: String<64>,
    pub stratum: Option<String<96>>,
    pub worker: Option<String<96>>,
    pub password: Option<String<64>>,
    pub wifi_ssid: Option<String<32>>,
    pub wifi_password: Option<String<64>>,
    pub cpu_mhz: Option<u8>,
    pub touch_map: Option<u8>,
    pub hash_focus: Option<bool>,
    pub reconnect: bool,
    pub reboot: bool,
}

/// Parse `application/x-www-form-urlencoded` body used by LAN + USB companion.
pub fn parse_companion_body(body: &str) -> CompanionUpdate {
    use crate::config::normalize_cpu_mhz;
    let mut upd = CompanionUpdate::default();
    for pair in body.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let raw = kv.next().unwrap_or("");
        let val = url_decode_simple(raw);
        match key {
            "auth" | "password_auth" | "current_password" => {
                upd.auth.clear();
                let _ = upd.auth.push_str(trunc(&val, 64));
            }
            "stratum" => {
                let mut s = String::new();
                let _ = s.push_str(trunc(&val, 96));
                upd.stratum = Some(s);
            }
            "worker" | "address" => {
                let mut s = String::new();
                let _ = s.push_str(trunc(&val, 96));
                upd.worker = Some(s);
            }
            "password" | "pool_password" => {
                let mut s = String::new();
                let _ = s.push_str(trunc(&val, 64));
                upd.password = Some(s);
            }
            "wifi_ssid" | "ssid" => {
                let mut s = String::new();
                let _ = s.push_str(trunc(&val, 32));
                upd.wifi_ssid = Some(s);
            }
            "wifi_password" | "wifi_pass" => {
                let mut s = String::new();
                let _ = s.push_str(trunc(&val, 64));
                upd.wifi_password = Some(s);
            }
            "cpu_mhz" | "clock" => {
                if let Ok(v) = val.parse::<u8>() {
                    upd.cpu_mhz = Some(normalize_cpu_mhz(v));
                    upd.reboot = true;
                }
            }
            "touch_map" => {
                if let Ok(v) = val.parse::<u8>() {
                    upd.touch_map = Some(v);
                }
            }
            "hash_focus" | "perf" => {
                upd.hash_focus = Some(val == "1" || val.eq_ignore_ascii_case("true"));
            }
            "reconnect" => {
                upd.reconnect = val == "1" || val.eq_ignore_ascii_case("true");
            }
            "reboot" => {
                upd.reboot = val == "1" || val.eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }
    upd
}

fn trunc(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

fn url_decode_simple(input: &str) -> heapless::String<128> {
    let mut out: heapless::String<128> = heapless::String::new();
    let b = input.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let ch = match b[i] {
            b'+' => {
                i += 1;
                ' '
            }
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                    i += 3;
                    (hi << 4 | lo) as char
                } else {
                    i += 1;
                    '%'
                }
            }
            c => {
                i += 1;
                c as char
            }
        };
        if out.push(ch).is_err() {
            break;
        }
    }
    out
}

#[cfg(feature = "esp")]
mod server {
    use alloc::format;

    use embassy_executor::Spawner;
    use embassy_net::tcp::TcpSocket;
    use embassy_net::Stack;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::mutex::Mutex;
    use embassy_time::{Duration, Timer};
    use log::info;

    use super::{CompanionUpdate, WebStatus};
    use crate::display::{DISPLAY_HEIGHT, DISPLAY_WIDTH};
    use crate::radio::WifiPhase;
    use crate::stratum::StratumPhase;

    const FW_VERSION: &str = env!("CARGO_PKG_VERSION");
    const MODEL: &str = "SCRYPT-CYD";
    const HOSTNAME: &str = "SCRYPT-CYD";

    static STATUS: Mutex<CriticalSectionRawMutex, WebStatus> = Mutex::new(WebStatus {
        hashrate_x100: 0,
        shares: 0,
        nonce: 0,
        address: heapless::String::new(),
        stratum: heapless::String::new(),
        wifi_ssid: heapless::String::new(),
        wifi: WifiPhase::Disabled,
        ip: None,
        pool_phase: StratumPhase::Disabled,
        accepted: 0,
        rejected: 0,
        dropped: 0,
        difficulty: 1,
        uptime_secs: 0,
        screen_on: true,
        cpu_mhz: 240,
        hash_focus: true,
    });

    static PENDING: Mutex<CriticalSectionRawMutex, Option<CompanionUpdate>> = Mutex::new(None);
    static WIFI_PASS_MASK: Mutex<CriticalSectionRawMutex, heapless::String<16>> =
        Mutex::new(heapless::String::new());

    pub fn publish(s: WebStatus) {
        if let Ok(mut slot) = STATUS.try_lock() {
            *slot = s;
        }
    }

    pub fn set_runtime_meta(cpu_mhz: u8, wifi_pass_masked: &str) {
        if let Ok(mut s) = STATUS.try_lock() {
            s.cpu_mhz = cpu_mhz;
        }
        if let Ok(mut m) = WIFI_PASS_MASK.try_lock() {
            m.clear();
            let _ = m.push_str(wifi_pass_masked);
        }
    }

    pub fn take_pending_update() -> Option<CompanionUpdate> {
        PENDING.try_lock().ok().and_then(|mut g| g.take())
    }

    pub fn start(spawner: &Spawner, stack: Stack<'static>) {
        match http_task(stack) {
            Ok(token) => {
                spawner.spawn(token);
                info!("web: listening on :80 (companion APIs enabled)");
            }
            Err(_) => info!("web: task token failed"),
        }
    }

    #[embassy_executor::task]
    async fn http_task(stack: Stack<'static>) {
        use static_cell::StaticCell;
        static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let rx_buf = RX_BUF.init([0; 1024]);
        let tx_buf = TX_BUF.init([0; 1024]);

        loop {
            stack.wait_config_up().await;

            let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
            socket.set_timeout(Some(Duration::from_secs(10)));

            if let Err(e) = socket.accept(80).await {
                info!("web: accept error {e:?}");
                Timer::after(Duration::from_millis(200)).await;
                continue;
            }

            let mut req = [0u8; 768];
            let mut got = 0usize;
            let deadline = embassy_time::Instant::now() + Duration::from_secs(3);
            while got < req.len() && embassy_time::Instant::now() < deadline {
                match socket.read(&mut req[got..]).await {
                    Ok(0) => break,
                    Ok(n) => {
                        got += n;
                        if req[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                            // For POST, try to read Content-Length body bytes too.
                            if let Some(need) = content_length(&req[..got]) {
                                let header_end = req[..got]
                                    .windows(4)
                                    .position(|w| w == b"\r\n\r\n")
                                    .map(|i| i + 4)
                                    .unwrap_or(got);
                                let have_body = got.saturating_sub(header_end);
                                if have_body >= need {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }

            let (method, path, body) = parse_request(&req[..got]);
            let snap = STATUS.lock().await.clone();

            let _ = match (method, path) {
                (_, "/probe") => write_probe(&mut socket, &snap).await,
                (_, "/alive") => write_alive(&mut socket, &snap).await,
                (_, "/api/system/info") => write_system_info(&mut socket, &snap).await,
                (_, "/api") | (_, "/api/") | (_, "/api/status") => {
                    write_json(&mut socket, &snap).await
                }
                ("GET", "/api/config") => write_config(&mut socket, &snap).await,
                ("POST", "/api/config") | ("POST", "/api/clock") | ("POST", "/api/reboot") => {
                    handle_companion_post(&mut socket, path, body).await
                }
                (_, "/api/reconnect") => {
                    crate::stratum::request_reconnect();
                    write_text(
                        &mut socket,
                        "200 OK",
                        "application/json",
                        "{\"ok\":true,\"reconnect\":true}\n",
                    )
                    .await
                }
                _ => write_html(&mut socket, &snap).await,
            };

            let _ = socket.flush().await;
            socket.close();
            Timer::after(Duration::from_millis(20)).await;
        }
    }

    async fn handle_companion_post(
        socket: &mut TcpSocket<'_>,
        path: &str,
        body: &str,
    ) -> Result<(), ()> {
        let mut upd = parse_update_body(body);
        if path.ends_with("reboot") {
            upd.reboot = true;
        }
        if path.ends_with("clock") && upd.cpu_mhz.is_none() {
            let _ = write_text(
                socket,
                "400 Bad Request",
                "application/json",
                "{\"ok\":false,\"error\":\"cpu_mhz required\"}\n",
            )
            .await;
            return Ok(());
        }
        if upd.auth.is_empty() {
            let _ = write_text(
                socket,
                "401 Unauthorized",
                "application/json",
                "{\"ok\":false,\"error\":\"auth (pool password) required\"}\n",
            )
            .await;
            return Ok(());
        }
        if let Ok(mut slot) = PENDING.try_lock() {
            *slot = Some(upd);
            write_text(
                socket,
                "200 OK",
                "application/json",
                "{\"ok\":true,\"queued\":true}\n",
            )
            .await
        } else {
            write_text(
                socket,
                "503 Service Unavailable",
                "application/json",
                "{\"ok\":false,\"error\":\"busy\"}\n",
            )
            .await
        }
    }

    fn content_length(req: &[u8]) -> Option<usize> {
        let Ok(s) = core::str::from_utf8(req) else {
            return None;
        };
        for line in s.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("content-length:") {
                return rest.trim().parse().ok();
            }
        }
        None
    }

    fn parse_request(req: &[u8]) -> (&str, &str, &str) {
        let Ok(s) = core::str::from_utf8(req) else {
            return ("GET", "/", "");
        };
        let mut lines = s.split("\r\n");
        let start = lines.next().unwrap_or("GET / HTTP/1.0");
        let mut parts = start.split_whitespace();
        let method = parts.next().unwrap_or("GET");
        let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
        let body = if let Some(idx) = s.find("\r\n\r\n") {
            &s[idx + 4..]
        } else {
            ""
        };
        (method, path, body)
    }

    fn parse_update_body(body: &str) -> CompanionUpdate {
        super::parse_companion_body(body)
    }

    async fn write_probe(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let hr = s.hashrate_x100 / 100;
        let body = format!(
            "{{\"model\":\"{MODEL}\",\"hostname\":\"{HOSTNAME}\",\"ver\":\"{FW_VERSION}\",\
\"sw\":{sw},\"sh\":{sh},\"hr\":{hr},\"sbd\":0,\"ebd\":0,\"ut\":{ut},\
\"algo\":\"scrypt\",\"board\":\"ESP32-2432S028\",\"cpu_mhz\":{cpu}}}",
            sw = DISPLAY_WIDTH,
            sh = DISPLAY_HEIGHT,
            hr = hr,
            ut = s.uptime_secs,
            cpu = s.cpu_mhz,
        );
        write_json_raw(socket, &body).await
    }

    async fn write_alive(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let self_ip = match s.ip {
            Some([a, b, c, d]) => format!("\"{a}.{b}.{c}.{d}\""),
            None => "null".into(),
        };
        let ips = match s.ip {
            Some([a, b, c, d]) => format!("[\"{a}.{b}.{c}.{d}\"]"),
            None => "[]".into(),
        };
        let body = format!("{{\"self\":{self_ip},\"ips\":{ips}}}");
        write_json_raw(socket, &body).await
    }

    async fn write_config(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let wifi_mask = WIFI_PASS_MASK
            .lock()
            .await
            .clone();
        let configured = !s.address.is_empty() && !s.stratum.is_empty() && !s.wifi_ssid.is_empty();
        let body = format!(
            "{{\"worker\":{w},\"stratum\":{st},\"wifi_ssid\":{ss},\"wifi_password\":\"{wm}\",\
\"cpu_mhz\":{cpu},\"hash_focus\":{hf},\"touch_map\":null,\"algo\":\"scrypt\",\"board\":\"ESP32-2432S028\",\
\"fw\":\"{FW_VERSION}\",\"screen_on\":{scr},\"configured\":{cfg}}}",
            w = json_str(s.address.as_str()),
            st = json_str(s.stratum.as_str()),
            ss = json_str(s.wifi_ssid.as_str()),
            wm = wifi_mask.as_str(),
            cpu = s.cpu_mhz,
            hf = if s.hash_focus { "true" } else { "false" },
            scr = if s.screen_on { "true" } else { "false" },
            cfg = if configured { "true" } else { "false" },
        );
        write_json_raw(socket, &body).await
    }

    async fn write_system_info(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let hr = format!("{}.{:02}", s.hashrate_x100 / 100, s.hashrate_x100 % 100);
        let body = format!(
            "{{\"identity\":{{\"hwModel\":\"{MODEL}\",\"hostName\":\"{HOSTNAME}\",\
\"fwVersion\":\"{FW_VERSION}\",\"board\":\"ESP32-2432S028\",\"algo\":\"scrypt\",\
\"cpuMhz\":{cpu}}},\"miner\":{{\"hashRate\":{hr},\"sAccepted\":{acc},\"sRejected\":{rej},\
\"dropped\":{drop},\"uptimeSeconds\":{ut},\"poolDiff\":{diff},\"shares\":{shares},\
\"nonce\":\"{nonce:08x}\",\"screenOn\":{screen}}},\"stratum\":{{\"url\":{url},\"user\":{user},\
\"phase\":\"{phase}\",\"connected\":{conn}}},\"wifi\":{{\"ssid\":{ssid},\"state\":\"{wifi}\",\
\"ip\":{ip}}}}}",
            cpu = s.cpu_mhz,
            hr = hr,
            acc = s.accepted,
            rej = s.rejected,
            drop = s.dropped,
            ut = s.uptime_secs,
            diff = s.difficulty,
            shares = s.shares,
            nonce = s.nonce,
            screen = if s.screen_on { "true" } else { "false" },
            url = json_str(s.stratum.as_str()),
            user = json_str(s.address.as_str()),
            phase = if s.pool_phase.is_connected() {
                "CONNECTED"
            } else {
                s.pool_phase.label()
            },
            conn = if s.pool_phase.is_connected() {
                "true"
            } else {
                "false"
            },
            ssid = json_str(s.wifi_ssid.as_str()),
            wifi = s.wifi.label(),
            ip = match s.ip {
                Some([a, b, c, d]) => format!("\"{a}.{b}.{c}.{d}\""),
                None => "null".into(),
            },
        );
        write_json_raw(socket, &body).await
    }

    async fn write_html(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let ip = match s.ip {
            Some([a, b, c, d]) => format!("{a}.{b}.{c}.{d}"),
            None => "—".into(),
        };
        let rate = format!("{}.{:02}", s.hashrate_x100 / 100, s.hashrate_x100 % 100);
        let connected = s.pool_phase.is_connected();
        let phase = if connected {
            "CONNECTED"
        } else {
            s.pool_phase.label()
        };
        let body = format!(
            "<!doctype html><html><head><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<meta http-equiv=refresh content=3>\
<title>SCRYPT · CYD</title>\
<style>\
body{{margin:0;font:15px/1.45 Georgia,'Times New Roman',serif;background:#101612;color:#e8f0e4}}\
header{{padding:1.2rem 1.4rem;background:linear-gradient(120deg,#1a2a1c,#142018);border-bottom:3px solid #c45c26}}\
h1{{margin:0;font-size:1.6rem;letter-spacing:.04em;color:#ff8c1a}}\
.sub{{color:#8aa08c;margin-top:.35rem}}\
main{{padding:1.2rem 1.4rem;display:grid;gap:.9rem;max-width:520px}}\
.card{{background:#1a241c;border-radius:12px;padding:1rem 1.1rem;border:1px solid #2a3a2c}}\
.k{{color:#8aa08c;font-size:.75rem;text-transform:uppercase;letter-spacing:.06em}}\
.v{{font-size:1.35rem;margin-top:.2rem;font-variant-numeric:tabular-nums}}\
.rate{{font-size:2.2rem;color:#ff8c1a}}\
a{{color:#7dffa0}}\
</style></head><body>\
<header><h1>SCRYPT</h1>\
<div class=sub>ESP32-2432S028 · {ip} · CPU {cpu} MHz</div></header>\
<main>\
<div class=card><div class=k>Active hashrate</div><div class=\"v rate\">{rate} H/s</div></div>\
<div class=card><div class=k>Pool</div><div class=v>{phase}</div>\
<div class=k style=margin-top:.6rem>acc {acc} / rej {rej} · {shares} shares</div></div>\
<div class=card><div class=k>Worker</div><div class=v style=font-size:1rem>{addr}</div></div>\
<div class=card>\
<a href=/api/status>JSON</a> · <a href=/api/config>config</a> · \
<a href=/probe>probe</a><br>\
<span style=color:#8aa08c;font-size:.85rem>Use the Windows CYD Companion for settings &amp; CPU clock</span>\
</div></main></body></html>",
            ip = ip,
            cpu = s.cpu_mhz,
            rate = rate,
            phase = phase,
            acc = s.accepted,
            rej = s.rejected,
            shares = s.shares,
            addr = html_escape(s.address.as_str()),
        );
        let header = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        write_all(socket, header.as_bytes()).await?;
        write_all(socket, body.as_bytes()).await
    }

    async fn write_text(
        socket: &mut TcpSocket<'_>,
        status: &str,
        ctype: &str,
        body: &str,
    ) -> Result<(), ()> {
        let header = format!(
            "HTTP/1.0 {status}\r\nContent-Type: {ctype}\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        write_all(socket, header.as_bytes()).await?;
        write_all(socket, body.as_bytes()).await
    }

    async fn write_json_raw(socket: &mut TcpSocket<'_>, body: &str) -> Result<(), ()> {
        write_text(socket, "200 OK", "application/json", body).await
    }

    async fn write_json(socket: &mut TcpSocket<'_>, s: &WebStatus) -> Result<(), ()> {
        let ip = match s.ip {
            Some([a, b, c, d]) => format!("\"{a}.{b}.{c}.{d}\""),
            None => "null".into(),
        };
        let pool = if s.pool_phase.is_connected() {
            "CONNECTED"
        } else {
            s.pool_phase.label()
        };
        let body = format!(
            "{{\"hashrate_hs\":{}.{:02},\"shares\":{},\"nonce\":\"{:08x}\",\
\"address\":{},\"stratum\":{},\"wifi\":\"{}\",\"ip\":{},\
\"pool\":\"{}\",\"connected\":{},\"accepted\":{},\"rejected\":{},\"dropped\":{},\
\"difficulty\":{},\"uptime_secs\":{},\"screen_on\":{},\"cpu_mhz\":{},\"hash_focus\":{}}}",
            s.hashrate_x100 / 100,
            s.hashrate_x100 % 100,
            s.shares,
            s.nonce,
            json_str(s.address.as_str()),
            json_str(s.stratum.as_str()),
            s.wifi.label(),
            ip,
            pool,
            if s.pool_phase.is_connected() {
                "true"
            } else {
                "false"
            },
            s.accepted,
            s.rejected,
            s.dropped,
            s.difficulty,
            s.uptime_secs,
            if s.screen_on { "true" } else { "false" },
            s.cpu_mhz,
            if s.hash_focus { "true" } else { "false" },
        );
        write_json_raw(socket, &body).await
    }

    async fn write_all(socket: &mut TcpSocket<'_>, mut data: &[u8]) -> Result<(), ()> {
        while !data.is_empty() {
            match socket.write(data).await {
                Ok(0) => return Err(()),
                Ok(n) => data = &data[n..],
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }

    fn html_escape(s: &str) -> alloc::string::String {
        let mut out = alloc::string::String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                _ => out.push(c),
            }
        }
        out
    }

    fn json_str(s: &str) -> alloc::string::String {
        let mut out = alloc::string::String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c if c < ' ' => {
                    let _ =
                        core::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(feature = "esp")]
pub use server::{publish, set_runtime_meta, start, take_pending_update};

#[cfg(not(feature = "esp"))]
pub fn publish(_s: WebStatus) {}

#[cfg(not(feature = "esp"))]
pub fn set_runtime_meta(_cpu_mhz: u8, _wifi_pass_masked: &str) {}

#[cfg(not(feature = "esp"))]
pub fn take_pending_update() -> Option<CompanionUpdate> {
    None
}
