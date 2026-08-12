//! Onboard WiFi (STA + DHCP) status / tasks.
//!
//! Status types are available on host builds; the radio stack only runs with `esp`.
//! Uses `esp-radio` + `embassy-net`. BLE is not started (classic ESP32 RAM goes to
//! WiFi / stratum / mining).

use core::fmt;
use heapless::String;

use crate::config::{WifiSsidString, WIFI_SSID_MAX};

/// Max networks kept after a setup scan (classic ESP32 RAM).
pub const WIFI_SCAN_MAX: usize = 8;

/// One AP from a setup-time WiFi scan (host + device).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedNetwork {
    pub ssid: WifiSsidString,
    pub rssi: i8,
    pub open: bool,
}

/// WiFi connection phase shown on the Radio tab / serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiPhase {
    Disabled,
    Starting,
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

impl WifiPhase {
    pub fn label(self) -> &'static str {
        match self {
            WifiPhase::Disabled => "off",
            WifiPhase::Starting => "start",
            WifiPhase::Connecting => "assoc",
            WifiPhase::Connected => "up",
            WifiPhase::Disconnected => "down",
            WifiPhase::Failed => "fail",
        }
    }
}

/// Snapshot of onboard radio state for GUI / serial.
#[derive(Clone, Debug)]
pub struct RadioStatus {
    pub wifi: WifiPhase,
    pub ssid: String<WIFI_SSID_MAX>,
    pub ip: Option<[u8; 4]>,
}

impl Default for RadioStatus {
    fn default() -> Self {
        Self {
            wifi: WifiPhase::Disabled,
            ssid: String::new(),
            ip: None,
        }
    }
}

impl RadioStatus {
    pub fn ip_string(&self) -> String<16> {
        let mut s = String::new();
        match self.ip {
            Some([a, b, c, d]) => {
                let _ = fmt::Write::write_fmt(&mut s, format_args!("{a}.{b}.{c}.{d}"));
            }
            None => {
                let _ = s.push_str("---");
            }
        }
        s
    }
}

#[cfg(feature = "esp")]
mod stack {
    use alloc::string::String as AllocString;

    use embassy_executor::Spawner;
    use embassy_net::{Runner, Stack, StackResources};
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::mutex::Mutex;
    use embassy_time::{Duration, Timer};
    use esp_hal::peripherals::WIFI;
    use esp_hal::rng::Rng;
    use esp_radio::wifi::{
        AuthenticationMethod, Config, ControllerConfig, Interface, PowerSaveMode, WifiController,
        scan::ScanConfig,
        sta::StationConfig,
    };
    use log::info;
    use static_cell::StaticCell;

    use super::{RadioStatus, ScannedNetwork, WifiPhase, WIFI_SCAN_MAX};
    use crate::config::{PoolConfig, WifiSsidString};

    static STATUS: Mutex<CriticalSectionRawMutex, RadioStatus> = Mutex::new(RadioStatus {
        wifi: WifiPhase::Disabled,
        ssid: heapless::String::new(),
        ip: None,
    });

    // DNS + stratum TCP + HTTP server (+ spare). Keep small — classic ESP32 RAM.
    static STACK_RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();

    fn spawn_task<S>(
        spawner: &Spawner,
        token: Result<embassy_executor::SpawnToken<S>, embassy_executor::SpawnError>,
        what: &str,
    ) {
        match token {
            Ok(t) => spawner.spawn(t),
            Err(_) => info!("failed to spawn {what}"),
        }
    }

    pub async fn snapshot() -> RadioStatus {
        STATUS.lock().await.clone()
    }

    async fn set_wifi_phase(phase: WifiPhase) {
        let mut s = STATUS.lock().await;
        s.wifi = phase;
        if phase != WifiPhase::Connected {
            s.ip = None;
        }
    }

    async fn set_ip(ip: Option<[u8; 4]>) {
        let mut s = STATUS.lock().await;
        s.ip = ip;
        if ip.is_some() {
            s.wifi = WifiPhase::Connected;
        }
    }

    fn seed_status(cfg: &PoolConfig) {
        if let Ok(mut s) = STATUS.try_lock() {
            s.wifi = if cfg.wifi_enabled() {
                WifiPhase::Starting
            } else {
                WifiPhase::Disabled
            };
            s.ssid.clear();
            let _ = s.ssid.push_str(cfg.wifi_ssid.as_str());
            s.ip = None;
        }
    }

    fn start_wifi(
        spawner: &Spawner,
        wifi: WIFI<'static>,
        cfg: &PoolConfig,
    ) -> Option<Stack<'static>> {
        let ssid = cfg.wifi_ssid.as_str();
        let password = cfg.wifi_password.as_str();

        let mut station = StationConfig::default().with_ssid(ssid);
        if password.is_empty() {
            station = station.with_auth_method(AuthenticationMethod::None);
        } else {
            // StationConfig::password is alloc::String; builder takes it by value.
            station = station.with_password(AllocString::from(password));
        }

        let wifi_interface = Interface::station();
        let mut controller = match WifiController::new(
            wifi,
            ControllerConfig::default().with_initial_config(Config::Station(station)),
        ) {
            Ok(c) => c,
            Err(e) => {
                info!("WiFi init failed: {e:?}");
                if let Ok(mut s) = STATUS.try_lock() {
                    s.wifi = WifiPhase::Failed;
                }
                return None;
            }
        };
        // Keep the radio fully awake — no modem sleep / DTIM power save.
        if let Err(e) = controller.set_power_saving(PowerSaveMode::None) {
            info!("WiFi power-save None failed: {e:?}");
        }

        let net_config = embassy_net::Config::dhcpv4(Default::default());
        let rng = Rng::new();
        let seed = (rng.random() as u64) << 32 | rng.random() as u64;
        let (stack, runner) = embassy_net::new(
            wifi_interface,
            net_config,
            STACK_RESOURCES.init(StackResources::<5>::new()),
            seed,
        );

        info!("WiFi starting for SSID={ssid}");
        spawn_task(spawner, connection(controller), "WiFi connection");
        spawn_task(spawner, net_task(runner), "net runner");
        spawn_task(spawner, dhcp_watch(stack), "DHCP watch");
        Some(stack)
    }

    /// Brief STA scan for setup UI.
    ///
    /// Uses `WIFI::steal()` so the caller's owned `WIFI` token can still be
    /// passed to [`start`] afterward (no token hand-off / second steal at boot).
    pub async fn scan_networks() -> Result<heapless::Vec<ScannedNetwork, WIFI_SCAN_MAX>, ()> {
        let wifi = unsafe { WIFI::steal() };
        let mut controller = match WifiController::new(wifi, ControllerConfig::default()) {
            Ok(c) => c,
            Err(e) => {
                info!("WiFi scan init failed: {e:?}");
                return Err(());
            }
        };

        let scan_config = ScanConfig::default().with_max(WIFI_SCAN_MAX.saturating_mul(2));
        let aps = match controller.scan_async(&scan_config).await {
            Ok(v) => v,
            Err(e) => {
                info!("WiFi scan failed: {e:?}");
                drop(controller);
                return Err(());
            }
        };

        let mut out: heapless::Vec<ScannedNetwork, WIFI_SCAN_MAX> = heapless::Vec::new();
        for ap in aps.iter() {
            let ssid_str = ap.ssid.as_str();
            if ssid_str.is_empty() {
                continue;
            }
            let open = matches!(ap.auth_method, Some(AuthenticationMethod::None) | None);
            // Dedupe SSID — keep strongest RSSI.
            if let Some(existing) = out.iter_mut().find(|n| n.ssid.as_str() == ssid_str) {
                if ap.signal_strength > existing.rssi {
                    existing.rssi = ap.signal_strength;
                    existing.open = open;
                }
                continue;
            }
            if out.is_full() {
                // Replace weakest if this one is stronger.
                if let Some((idx, weakest)) = out
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, n)| n.rssi)
                {
                    if ap.signal_strength > weakest.rssi {
                        let mut ssid = WifiSsidString::new();
                        let _ = ssid.push_str(ssid_str);
                        out[idx] = ScannedNetwork {
                            ssid,
                            rssi: ap.signal_strength,
                            open,
                        };
                    }
                }
                continue;
            }
            let mut ssid = WifiSsidString::new();
            if ssid.push_str(ssid_str).is_err() {
                continue;
            }
            let _ = out.push(ScannedNetwork {
                ssid,
                rssi: ap.signal_strength,
                open,
            });
        }
        out.sort_unstable_by(|a, b| b.rssi.cmp(&a.rssi));
        drop(aps);
        drop(controller);
        info!("WiFi scan found {} network(s)", out.len());
        Ok(out)
    }

    /// Start WiFi STA + DHCP (SSID is required by setup / saved config).
    ///
    /// Returns the embassy-net [`Stack`] so callers can open TCP (stratum) sockets.
    pub fn start(
        spawner: &Spawner,
        wifi: WIFI<'static>,
        cfg: &PoolConfig,
    ) -> Option<Stack<'static>> {
        seed_status(cfg);

        if !cfg.wifi_enabled() {
            info!("WiFi missing SSID — refusing to start");
            let _ = wifi;
            return None;
        }
        start_wifi(spawner, wifi, cfg)
    }

    #[embassy_executor::task]
    async fn connection(mut controller: WifiController<'static>) {
        info!("WiFi connection task (always-on, no power save)");
        loop {
            let _ = controller.set_power_saving(PowerSaveMode::None);
            set_wifi_phase(WifiPhase::Connecting).await;
            match controller.connect_async().await {
                Ok(info) => {
                    info!("WiFi connected: {info:?}");
                    let _ = controller.set_power_saving(PowerSaveMode::None);
                    set_wifi_phase(WifiPhase::Connected).await;
                    let _ = controller.wait_for_disconnect_async().await;
                    info!("WiFi disconnected — reconnecting");
                    set_wifi_phase(WifiPhase::Disconnected).await;
                    // Brief pause then retry immediately so STA stays up.
                    Timer::after(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    info!("WiFi connect failed: {e:?} — retry");
                    set_wifi_phase(WifiPhase::Failed).await;
                    Timer::after(Duration::from_secs(2)).await;
                }
            }
        }
    }

    #[embassy_executor::task]
    async fn net_task(mut runner: Runner<'static, Interface>) {
        runner.run().await
    }

    #[embassy_executor::task]
    async fn dhcp_watch(stack: embassy_net::Stack<'static>) {
        loop {
            stack.wait_config_up().await;
            if let Some(cfg) = stack.config_v4() {
                let octets = cfg.address.address().octets();
                info!(
                    "DHCP got IP {}.{}.{}.{}",
                    octets[0], octets[1], octets[2], octets[3]
                );
                set_ip(Some(octets)).await;
            }
            loop {
                Timer::after(Duration::from_secs(2)).await;
                match stack.config_v4() {
                    Some(cfg) => {
                        set_ip(Some(cfg.address.address().octets())).await;
                    }
                    None => {
                        set_ip(None).await;
                        set_wifi_phase(WifiPhase::Disconnected).await;
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "esp")]
pub use stack::{scan_networks, snapshot, start};
