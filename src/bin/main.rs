//! ESP32 scrypt miner firmware for **ESP32-2432S028** (Cheap Yellow Display).
//!
//! After first boot, enter optional **WiFi**, then **stratum** / **worker** /
//! **password** over touch or USB serial (CH340 UART0). Values are saved to
//! flash and auto-loaded on later boots. Onboard WiFi (STA+DHCP) starts from
//! those settings. When WiFi is configured, a **stratum TCP client** connects
//! to the pool and mines real jobs. BLE is not used (keeps RAM for mining).
//!
//! Controls: **touch** tabs/menu/on-screen keyboard; **BOOT** short=next tab,
//! long=menu. Serial still accepts `change` / `radio` / `stratum`.
//!
//! Flash (ESP Rust toolchain + espflash required):
//! ```text
//! cargo +esp run -Zbuild-std=core,alloc --release \
//!   --target xtensa-esp32-none-elf --features esp,lite
//! ```

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe with esp_hal types"
)]

extern crate alloc;

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_io::Write;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pin, Pull};
use esp_hal::ram;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::Blocking;
use heapless::String;
use log::info;

use esp32_s3_scrypt_miner::config::{
    normalize_cpu_mhz, ConfigError, PoolConfig, SetupField, DEFAULT_STRATUM,
};
use esp32_s3_scrypt_miner::touch::TouchMap;
use esp32_s3_scrypt_miner::display::{Display, DisplayPeripherals};
use esp32_s3_scrypt_miner::gui::GuiState;
use esp32_s3_scrypt_miner::gui::GuiScreen;
use esp32_s3_scrypt_miner::keyboard::{hit_gui, hit_wifi_scan, GuiHit, Keyboard, WifiScanHit};
use esp32_s3_scrypt_miner::miner::ScryptMiner;
use esp32_s3_scrypt_miner::persist::ConfigStore;
use esp32_s3_scrypt_miner::radio::{self, RadioStatus, ScannedNetwork};
use esp32_s3_scrypt_miner::stratum::{self, JobMeta, StratumPhase, StratumStatus};
use esp32_s3_scrypt_miner::touch::{Touch, TouchPins};
use esp32_s3_scrypt_miner::web::{self, WebStatus};
use esp_hal::peripherals::WIFI;

esp_bootloader_esp_idf::esp_app_desc!();

/// Hashes between USB/GUI checks. Keep modest so companion `cmp *` stays snappy.
const BATCH_SIZE: usize = 2;
const DEMO_ZERO_NIBBLES: u8 = 4;
const SAVED_CONFIRM_SECS: u64 = 8;
const MAX_PASSWORD_ATTEMPTS: u8 = 3;
const BOOT_LONG_PRESS_MS: u64 = 700;
/// GUI redraw interval when not in hash-focus (LCD stays on).
const GUI_REFRESH_MS: u64 = 5000;
/// Hash-focus: rare full redraws — footer H/s still updates more often.
const GUI_REFRESH_FOCUS_MS: u64 = 30_000;
/// How often to refresh stats / radio / web publish (ms). Higher = more mining time.
const STATUS_WINDOW_MS: u64 = 2000;
/// NMMiner: if WiFi stays down this long, soft-reset the chip to recover the radio.
const WIFI_DOWN_RESET_SECS: u64 = 600;

type Serial<'d> = Uart<'d, Blocking>;

/// Survives soft-reset so a saved CPU MHz can be applied on the next boot.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut BOOT_CLK_WORD: u32 = 0;
const BOOT_CLK_MAGIC: u32 = 0xC10C_A500;
const BOOT_CLK_RETRY: u32 = 0x0001_0000;

fn stash_boot_cpu_mhz(mhz: u8) {
    let m = u32::from(normalize_cpu_mhz(mhz));
    unsafe {
        BOOT_CLK_WORD = BOOT_CLK_MAGIC | m;
    }
}

fn take_boot_cpu_clock() -> (CpuClock, u8, bool) {
    let word = unsafe { BOOT_CLK_WORD };
    let valid = (word & 0xFFFF_FF00) == BOOT_CLK_MAGIC;
    let mhz = if valid {
        normalize_cpu_mhz((word & 0xFF) as u8)
    } else {
        240
    };
    let retry = valid && (word & BOOT_CLK_RETRY) != 0;
    let clock = match mhz {
        80 => CpuClock::_80MHz,
        160 => CpuClock::_160MHz,
        _ => CpuClock::_240MHz,
    };
    (clock, mhz, retry)
}

fn mark_boot_clk_retry(mhz: u8) {
    let m = u32::from(normalize_cpu_mhz(mhz));
    unsafe {
        BOOT_CLK_WORD = BOOT_CLK_MAGIC | BOOT_CLK_RETRY | m;
    }
}

fn clear_boot_clk_retry(mhz: u8) {
    stash_boot_cpu_mhz(mhz);
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // Keep UART0 quiet for the Windows companion — INFO spam was starving `cmp status`.
    esp_println::logger::init_logger(log::LevelFilter::Error);
    info!("esp32-2432s028 scrypt miner starting");

    let (cpu_clock, running_mhz, boot_retry) = take_boot_cpu_clock();
    info!("cpu clock target {running_mhz} MHz (retry={boot_retry})");
    let config = esp_hal::Config::default().with_cpu_clock(cpu_clock);
    let peripherals = esp_hal::init(config);

    // Classic ESP32: WiFi STA alone wants ~47–57 KiB. Use bootloader-reclaimed
    // DRAM for the radio blobs, plus a smaller .bss heap for app buffers
    // (lite Litecoin scrypt TMTO ≈ 8 KiB, embassy-net, etc.). Keep the .bss heap lean so
    // the linker can leave a larger ProCpu stack (WiFi IRQs share that stack).
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 72 * 1024);
    esp_alloc::heap_allocator!(size: 16 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    // CYD USB-UART bridge on UART0 (GPIO1 TX / GPIO3 RX).
    let mut usb = Uart::new(peripherals.UART0, UartConfig::default())
        .expect("UART0")
        .with_tx(peripherals.GPIO1)
        .with_rx(peripherals.GPIO3);

    let mut store = ConfigStore::new(peripherals.FLASH);

    // Only physical user button is BOOT (GPIO0). Short = next, long = action.
    let boot_btn = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    // CYD ILI9341 HSPI pins.
    let dp = DisplayPeripherals {
        spi: peripherals.SPI2,
        sclk: peripherals.GPIO14.degrade(),
        mosi: peripherals.GPIO13.degrade(),
        miso: peripherals.GPIO12.degrade(),
        cs: peripherals.GPIO15.degrade(),
        dc: peripherals.GPIO2.degrade(),
        backlight: peripherals.GPIO21.degrade(),
    };

    let mut display = match Display::new(dp, Delay::new()) {
        Ok(d) => d,
        Err(e) => {
            info!("display init failed: {e}");
            serial_writeln(&mut usb, "display init failed");
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    };

    // XPT2046 on dedicated VSPI (SPI3) — ESPHome CYD pinout @ 1 MHz.
    let mut touch = Touch::new(TouchPins {
        spi: peripherals.SPI3,
        clk: peripherals.GPIO25.degrade(),
        mosi: peripherals.GPIO32.degrade(),
        miso: peripherals.GPIO39.degrade(),
        cs: peripherals.GPIO33.degrade(),
        irq: peripherals.GPIO36.degrade(),
    });
    let mut touch_delay = Delay::new();
    serial_writeln(
        &mut usb,
        "Touch: SPI3 XPT2046 2MHz CLK25/MOSI32/MISO39/CS33/IRQ36",
    );

    if let Ok(saved) = store.load() {
        touch.map = TouchMap::from_id(saved.touch_map);
    }
    serial_write(&mut usb, "Touch map: ");
    serial_writeln(&mut usb, touch.map.label());
    serial_writeln(&mut usb, "Serial 'touch' cycles axis map (saved to flash).");

    let _ = display.draw_splash();
    Timer::after(Duration::from_millis(200)).await;

    // Require a sustained BOOT hold so reset glitches don't trap the board in
    // classic serial setup (which ate companion `cmp status` as field text).
    let force_change = boot_hold_ms(&boot_btn, 1_500).await;
    if force_change {
        serial_writeln(&mut usb, "BOOT held 1.5s — on-device setup mode.");
    }

    let mut wifi_token: Option<WIFI<'static>> = Some(peripherals.WIFI);
    let (mut pool, from_flash) = resolve_pool_config(
        &mut usb,
        &mut display,
        &mut touch,
        &mut touch_delay,
        &mut store,
        force_change,
        &mut wifi_token,
        &boot_btn,
    )
    .await;
    pool.touch_map = touch.map.id();
    pool.cpu_mhz = normalize_cpu_mhz(pool.cpu_mhz);
    // Apply saved CPU profile across soft-reset (companion overclock / underclock).
    if pool.cpu_mhz != running_mhz {
        if boot_retry {
            info!(
                "cpu mhz flash={} running={} — keeping running after retry",
                pool.cpu_mhz, running_mhz
            );
            clear_boot_clk_retry(running_mhz);
            pool.cpu_mhz = running_mhz;
            let _ = store.save(&pool);
        } else {
            serial_write(&mut usb, "Applying CPU ");
            let mut m: String<8> = String::new();
            let _ = core::fmt::Write::write_fmt(&mut m, format_args!("{} MHz", pool.cpu_mhz));
            serial_write(&mut usb, m.as_str());
            serial_writeln(&mut usb, " — soft reset…");
            mark_boot_clk_retry(pool.cpu_mhz);
            Timer::after(Duration::from_millis(150)).await;
            esp_hal::system::software_reset();
        }
    } else {
        clear_boot_clk_retry(pool.cpu_mhz);
    }
    web::set_runtime_meta(running_mhz, pool.wifi_password_masked().as_str());

    if pool.is_complete() {
        let _ = display.draw_config_summary(&pool, from_flash);
    } else {
        let _ = display.draw_waiting_companion();
    }
    serial_writeln(&mut usb, "");
    if from_flash && pool.is_complete() {
        serial_writeln(&mut usb, "Credentials in flash (will auto-load on reboot).");
        serial_writeln(
            &mut usb,
            "GUI: tap tabs · BOOT short=tabs, long=menu · serial: change",
        );
    } else if !pool.is_complete() {
        serial_writeln(
            &mut usb,
            "No credentials — waiting for CYD Companion over USB.",
        );
        serial_writeln(
            &mut usb,
            "App: Connect → Setup → fill WiFi + Pool → Save & reboot.",
        );
        serial_writeln(
            &mut usb,
            "Fallback: hold BOOT at power-on for on-device touch/serial setup.",
        );
    } else {
        serial_writeln(
            &mut usb,
            "WARNING: credentials NOT in flash — save from companion.",
        );
    }
    print_config_serial(&mut usb, &pool);

    // Reserve scrypt buffers *before* WiFi eats the heap (OOM panic otherwise).
    serial_writeln(&mut usb, "Allocating miner buffers…");
    let mut miner = ScryptMiner::new_demo(DEMO_ZERO_NIBBLES);
    serial_writeln(&mut usb, "Starting radio…");

    // Scan uses WIFI::steal internally; start uses the owned token.
    let wifi = wifi_token
        .take()
        .unwrap_or_else(|| unsafe { WIFI::steal() });

    let stratum_enabled = if let Some(stack) = radio::start(&spawner, wifi, &pool) {

        stratum::start(&spawner, stack, &pool);
        web::start(&spawner, stack);
        serial_writeln(&mut usb, "Stratum client starting (needs WiFi + DHCP).");
        serial_writeln(
            &mut usb,
            "Web UI on port 80 after DHCP — open http://<board-ip>/",
        );
        true
    } else {
        serial_writeln(
            &mut usb,
            "WiFi failed to start — check SSID/password (change + reboot).",
        );
        false
    };

    // Brief connecting screen only — mining loop shows ONLINE without stalling.
    if stratum_enabled {
        let _ = display.draw_connecting(pool.wifi_ssid.as_str());
        Timer::after(Duration::from_millis(400)).await;
    } else {
        Timer::after(Duration::from_millis(400)).await;
    }

    info!(
        "miner ready (N={}, log_n={}) stratum={} wifi={} from_flash={}",
        esp32_s3_scrypt_miner::SCRYPT_N,
        esp32_s3_scrypt_miner::SCRYPT_LOG_N,
        pool.stratum,
        pool.wifi_ssid,
        from_flash
    );
    let mut active_job: Option<JobMeta> = None;
    let mut pool_mode = false;
    let mut window_start = Instant::now();
    let mut window_hashes: u64 = 0;
    let mut last_hashrate_x100: u32 = 0;
    // Wide enough for USB companion `cmp set …` form bodies.
    let mut cmd_line: String<384> = String::new();
    let mut gui = GuiState::default();
    let mut boot_was_down = boot_btn.is_low();
    let mut boot_down_since: Option<Instant> = None;
    let mut saw_ip = false;
    let mut saw_stratum = false;
    // Non-blocking full-screen banner (ONLINE / CONNECTED); mining continues.
    let mut banner_until: Option<Instant> = None;
    let boot_at = Instant::now();
    let mut last_gui = Instant::now();
    // When we last had a DHCP IP (for NMMiner-style 10 min recovery reset).
    let mut wifi_ok_at = Instant::now();

    let mut stats = miner.stats();
    let mut radio_status = radio::snapshot().await;
    let mut stratum_status = if stratum_enabled {
        stratum::snapshot().await
    } else {
        StratumStatus::default()
    };
    if radio_status.ip.is_some() {
        saw_ip = true;
    }
    // Prefer MINE so live H/s is visible once hashing starts.
    gui.screen = GuiScreen::Mining;
    let _ = display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);

    loop {
        if let Some(job) = stratum::try_take_job() {
            let diff = job.difficulty;
            let clean = job.clean;
            let job_id = job.meta.job_id.clone();
            info!("new stratum job={job_id} diff={diff} clean={clean}");
            miner.set_job(job.header, job.target, 0);
            active_job = Some(job.meta);
            pool_mode = true;
            // Skip UART chatter in hash-focus — mining CPU first.
            if !pool.hash_focus {
                let mut m: String<96> = String::new();
                let _ = core::fmt::Write::write_fmt(
                    &mut m,
                    format_args!("JOB {job_id} diff={diff}"),
                );
                serial_writeln(&mut usb, m.as_str());
            }
        }

        let boot_down = boot_btn.is_low();
        if boot_down && !boot_was_down {
            boot_down_since = Some(Instant::now());
        }
        if !boot_down && boot_was_down {
            if let Some(start) = boot_down_since.take() {
                let long = start.elapsed() >= Duration::from_millis(BOOT_LONG_PRESS_MS);
                if long {
                    gui.on_action_press();
                    if gui.take_change_request() {
                        if let Some(updated) = password_gated_change(
                            &mut usb,
                            &mut display,
                            &mut touch,
                            &mut touch_delay,
                            &mut store,
                            &pool,
                            &mut None,
                            &boot_btn,
                        )
                        .await
                        {
                            pool = updated;
                            if stratum_enabled {
                                stratum::apply_pool_config(&pool).await;
                                serial_writeln(
                                    &mut usb,
                                    "Stratum worker/endpoint reloaded. WiFi still needs reboot.",
                                );
                            } else {
                                serial_writeln(
                                    &mut usb,
                                    "Note: WiFi keeps prior session until reboot.",
                                );
                            }
                            gui.screen = esp32_s3_scrypt_miner::gui::GuiScreen::Config;
                            let _ = display.draw_config_summary(&pool, true);
                            Timer::after(Duration::from_secs(2)).await;
                        }
                        gui.screen = esp32_s3_scrypt_miner::gui::GuiScreen::Mining;
                    }
                    display.invalidate();
                    radio_status = radio::snapshot().await;
                    if stratum_enabled {
                        stratum_status = stratum::snapshot().await;
                    }
                    let _ =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);
                    last_gui = Instant::now();
                } else {
                    gui.on_boot_short_press();
                    display.invalidate();
                    radio_status = radio::snapshot().await;
                    if stratum_enabled {
                        stratum_status = stratum::snapshot().await;
                    }
                    let _ =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);
                    last_gui = Instant::now();
                }
            }
        }
        boot_was_down = boot_down;

        // Touch: tab strip, menu rows, config "change" band.
        if let Some(p) = touch.poll_tap(&mut touch_delay) {
            let on_menu = gui.screen == esp32_s3_scrypt_miner::gui::GuiScreen::Menu;
            match hit_gui(p, on_menu) {
                Some(GuiHit::Tab(i)) => {
                    gui.set_tab(i);
                    display.invalidate();
                    let _ =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);
                }
                Some(GuiHit::MenuRow(i)) => {
                    gui.select_menu_row(i);
                    gui.activate_menu();
                    if gui.take_change_request() {
                        if let Some(updated) = password_gated_change(
                            &mut usb,
                            &mut display,
                            &mut touch,
                            &mut touch_delay,
                            &mut store,
                            &pool,
                            &mut None,
                            &boot_btn,
                        )
                        .await
                        {
                            pool = updated;
                            if stratum_enabled {
                                stratum::apply_pool_config(&pool).await;
                            }
                            let _ = display.draw_config_summary(&pool, true);
                            Timer::after(Duration::from_secs(2)).await;
                        }
                        gui.screen = esp32_s3_scrypt_miner::gui::GuiScreen::Mining;
                    }
                    display.invalidate();
                    let _ =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);
                }
                Some(GuiHit::ChangeBanner)
                    if gui.screen == esp32_s3_scrypt_miner::gui::GuiScreen::Config =>
                {
                    if let Some(updated) = password_gated_change(
                        &mut usb,
                        &mut display,
                        &mut touch,
                        &mut touch_delay,
                        &mut store,
                        &pool,
                        &mut None,
                        &boot_btn,
                    )
                    .await
                    {
                        pool = updated;
                        if stratum_enabled {
                            stratum::apply_pool_config(&pool).await;
                        }
                        let _ = display.draw_config_summary(&pool, true);
                        Timer::after(Duration::from_secs(2)).await;
                    }
                    gui.screen = esp32_s3_scrypt_miner::gui::GuiScreen::Mining;
                    display.invalidate();
                    let _ =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true);
                }
                _ => {}
            }
        }

        // Service USB companion before hashing (and again after — see below).
        service_usb_commands(
            &mut usb,
            &mut cmd_line,
            &mut store,
            &mut pool,
            &mut touch,
            &mut display,
            &mut touch_delay,
            &boot_btn,
            &stats,
            last_hashrate_x100,
            &mut radio_status,
            &mut stratum_status,
            running_mhz,
            stratum_enabled,
            &mut gui,
        )
        .await;

        let (last, found_share) = miner.mine_batch(BATCH_SIZE);
        window_hashes = window_hashes.saturating_add(BATCH_SIZE as u64);

        // Drain companion cmds that arrived during the scrypt batch.
        service_usb_commands(
            &mut usb,
            &mut cmd_line,
            &mut store,
            &mut pool,
            &mut touch,
            &mut display,
            &mut touch_delay,
            &boot_btn,
            &stats,
            last_hashrate_x100,
            &mut radio_status,
            &mut stratum_status,
            running_mhz,
            stratum_enabled,
            &mut gui,
        )
        .await;

        if found_share {
            if pool_mode {
                if let Some(meta) = active_job.as_ref() {
                    let share = stratum::make_share(pool.address.as_str(), meta, last.nonce);
                    stratum::queue_share(share).await;
                }
            }
        }

        let elapsed = window_start.elapsed();
        if elapsed >= Duration::from_millis(STATUS_WINDOW_MS) {
            let ms = elapsed.as_millis().max(1);
            let hashrate_x100 = ((window_hashes as u128 * 100_000) / u128::from(ms)) as u32;
            last_hashrate_x100 = hashrate_x100;

            stats = miner.stats();
            stats.hashrate_x100 = hashrate_x100;
            radio_status = radio::snapshot().await;
            service_usb_commands(
                &mut usb,
                &mut cmd_line,
                &mut store,
                &mut pool,
                &mut touch,
                &mut display,
                &mut touch_delay,
                &boot_btn,
                &stats,
                last_hashrate_x100,
                &mut radio_status,
                &mut stratum_status,
                running_mhz,
                stratum_enabled,
                &mut gui,
            )
            .await;
            if stratum_enabled {
                stratum_status = stratum::snapshot().await;
            }
            // NMMiner-style recovery: soft-reset if we never get / lose DHCP for 10 min.
            if radio_status.ip.is_some() {
                wifi_ok_at = Instant::now();
            } else if stratum_enabled
                && wifi_ok_at.elapsed() >= Duration::from_secs(WIFI_DOWN_RESET_SECS)
            {
                serial_writeln(
                    &mut usb,
                    "WiFi/DHCP down >10min — soft reset (NMMiner-style recovery)",
                );
                info!("WiFi/DHCP down >{WIFI_DOWN_RESET_SECS}s — software_reset");
                Timer::after(Duration::from_millis(200)).await;
                esp_hal::system::software_reset();
            }
            if !saw_ip {
                if let Some(ip) = radio_status.ip {
                    saw_ip = true;
                    serial_write(&mut usb, "IP address: ");
                    serial_writeln(&mut usb, radio_status.ip_string().as_str());
                    if !pool.hash_focus {
                        let _ = display.draw_online(pool.wifi_ssid.as_str(), ip);
                        banner_until = Some(Instant::now() + Duration::from_secs(1));
                    }
                    gui.screen = GuiScreen::Mining;
                }
            }
            if !saw_stratum && stratum_status.phase.is_connected() {
                saw_stratum = true;
                if !pool.hash_focus {
                    serial_writeln(&mut usb, "Stratum CONNECTED — mining continues");
                    let _ = display.draw_pool_connected(pool.stratum.as_str(), hashrate_x100);
                    banner_until = Some(Instant::now() + Duration::from_secs(1));
                }
                gui.screen = GuiScreen::Mining;
            }
            if saw_stratum
                && !matches!(
                    stratum_status.phase,
                    StratumPhase::Idle | StratumPhase::Mining | StratumPhase::Disabled
                )
            {
                // Allow re-announce after reconnect.
                if matches!(
                    stratum_status.phase,
                    StratumPhase::Error
                        | StratumPhase::Connecting
                        | StratumPhase::WaitingWifi
                ) {
                    saw_stratum = false;
                }
            }
            let banner_active = banner_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false);
            if banner_active {
                // Full-screen ONLINE / CONNECTED banner — skip normal GUI redraw.
            } else {
                banner_until = None;
                let gui_ms = if pool.hash_focus {
                    GUI_REFRESH_FOCUS_MS
                } else {
                    GUI_REFRESH_MS
                };
                if last_gui.elapsed() >= Duration::from_millis(gui_ms) {
                    if let Err(e) =
                        display.draw_gui(&gui, &stats, &pool, &radio_status, &stratum_status, true)
                    {
                        info!("display error: {e}");
                    }
                    last_gui = Instant::now();
                } else if pool.hash_focus {
                    // Cheap live H/s only — avoid full LCD paint while hashing.
                    let _ = display.draw_hashrate_footer(hashrate_x100);
                }
            }

            {
                let mut ws = WebStatus::default();
                ws.hashrate_x100 = hashrate_x100;
                ws.shares = stats.shares;
                ws.nonce = stats.nonce;
                let _ = ws.address.push_str(pool.address.as_str());
                let _ = ws.stratum.push_str(pool.stratum.as_str());
                let _ = ws.wifi_ssid.push_str(pool.wifi_ssid.as_str());
                ws.wifi = radio_status.wifi;
                ws.ip = radio_status.ip;
                ws.pool_phase = stratum_status.phase;
                ws.accepted = stratum_status.accepted;
                ws.rejected = stratum_status.rejected;
                ws.dropped = stratum_status.dropped;
                ws.difficulty = stratum_status.difficulty;
                ws.uptime_secs = boot_at.elapsed().as_secs();
                ws.screen_on = true;
                ws.cpu_mhz = running_mhz;
                ws.hash_focus = pool.hash_focus;
                web::publish(ws);
            }

            if let Some(upd) = web::take_pending_update() {
                apply_companion_update(
                    &mut usb,
                    &mut store,
                    &mut pool,
                    &mut touch,
                    upd,
                    stratum_enabled,
                    running_mhz,
                )
                .await;
            }

            // Do not log H/s on UART0 — companion owns this link (CMPSTATUS).
            window_start = Instant::now();
            window_hashes = 0;
        }

        // Yield to WiFi/stratum without sleeping a full millisecond each batch.
        Timer::after(Duration::from_millis(0)).await;
    }
}

/// Drain UART RX and handle companion / serial commands. Called often so `cmp status`
/// cannot sit behind a scrypt batch or radio snapshot.
async fn service_usb_commands<D: embedded_hal::delay::DelayNs>(
    usb: &mut Serial<'_>,
    cmd_line: &mut String<384>,
    store: &mut ConfigStore<'_>,
    pool: &mut PoolConfig,
    touch: &mut Touch,
    display: &mut Display<'_, D>,
    touch_delay: &mut Delay,
    boot_btn: &Input<'_>,
    stats: &esp32_s3_scrypt_miner::miner::MinerStats,
    last_hashrate_x100: u32,
    radio_status: &mut RadioStatus,
    stratum_status: &mut StratumStatus,
    running_mhz: u8,
    stratum_enabled: bool,
    gui: &mut GuiState,
) {
    while poll_command_line(usb, cmd_line) {
        let cmd = cmd_line.as_str().trim();
        if let Some(rest) = strip_cmp_prefix(cmd) {
            handle_cmp_command(
                usb,
                store,
                pool,
                touch,
                rest,
                stats,
                last_hashrate_x100,
                radio_status,
                stratum_status,
                running_mhz,
                stratum_enabled,
            )
            .await;
        } else if is_change_command(cmd) {
            if let Some(updated) = password_gated_change(
                usb,
                display,
                touch,
                touch_delay,
                store,
                pool,
                &mut None,
                boot_btn,
            )
            .await
            {
                *pool = updated;
                if stratum_enabled {
                    stratum::apply_pool_config(pool).await;
                    serial_writeln(
                        usb,
                        "Stratum worker/endpoint reloaded. WiFi still needs reboot.",
                    );
                } else {
                    serial_writeln(usb, "Note: WiFi keeps prior session until reboot.");
                }
                let _ = display.draw_config_summary(pool, true);
                Timer::after(Duration::from_secs(2)).await;
            }
            gui.screen = GuiScreen::Mining;
            *radio_status = radio::snapshot().await;
            if stratum_enabled {
                *stratum_status = stratum::snapshot().await;
            }
            display.invalidate();
            let _ = display.draw_gui(gui, stats, pool, radio_status, stratum_status, true);
        } else if is_radio_command(cmd) {
            *radio_status = radio::snapshot().await;
            if stratum_enabled {
                *stratum_status = stratum::snapshot().await;
            }
            print_radio_serial(usb, pool, radio_status, stratum_status);
            gui.screen = GuiScreen::Radio;
            let _ = display.draw_gui(gui, stats, pool, radio_status, stratum_status, true);
        } else if is_touch_command(cmd) {
            touch.cycle_map();
            pool.touch_map = touch.map.id();
            match store.save(pool) {
                Ok(()) => {
                    serial_write(usb, "Touch map → ");
                    serial_write(usb, touch.map.label());
                    serial_writeln(usb, " (saved)");
                }
                Err(_) => {
                    serial_write(usb, "Touch map → ");
                    serial_write(usb, touch.map.label());
                    serial_writeln(usb, " (save failed)");
                }
            }
        } else if !cmd.is_empty() {
            // Stay quiet for garbage / log echo — only answer real cmp verbs above.
        }
        cmd_line.clear();
    }
}

fn print_config_serial(usb: &mut Serial<'_>, pool: &PoolConfig) {
    // Same top→bottom order as CONF GUI / setup.
    serial_write(usb, "  wifi_ssid = ");
    if pool.wifi_enabled() {
        serial_writeln(usb, pool.wifi_ssid.as_str());
    } else {
        serial_writeln(usb, "(disabled)");
    }
    serial_write(usb, "  wifi_password = ");
    serial_writeln(usb, pool.wifi_password_masked().as_str());
    serial_write(usb, "  stratum  = ");
    serial_writeln(usb, pool.stratum.as_str());
    serial_write(usb, "  worker  = ");
    serial_writeln(usb, pool.address.as_str());
    serial_write(usb, "  password = ");
    serial_writeln(usb, pool.password_masked().as_str());
}

fn print_radio_serial(
    usb: &mut Serial<'_>,
    pool: &PoolConfig,
    radio: &RadioStatus,
    stratum: &StratumStatus,
) {
    serial_writeln(usb, "");
    serial_writeln(usb, "=== Radio / stratum status ===");
    serial_write(usb, "  wifi = ");
    serial_write(usb, radio.wifi.label());
    serial_write(usb, "  ssid = ");
    if pool.wifi_enabled() {
        serial_writeln(usb, pool.wifi_ssid.as_str());
    } else {
        serial_writeln(usb, "(disabled)");
    }
    serial_write(usb, "  ip = ");
    serial_writeln(usb, radio.ip_string().as_str());
    serial_write(usb, "  stratum = ");
    serial_write(usb, stratum.phase.label());
    serial_write(usb, "  endpoint = ");
    serial_writeln(usb, pool.stratum.as_str());
    serial_write(usb, "  difficulty = ");
    let mut diff: String<16> = String::new();
    let _ = core::fmt::Write::write_fmt(&mut diff, format_args!("{}", stratum.difficulty));
    serial_writeln(usb, diff.as_str());
    serial_write(usb, "  accepted/rejected/dropped = ");
    let mut ar: String<32> = String::new();
    let _ = core::fmt::Write::write_fmt(
        &mut ar,
        format_args!(
            "{}/{}/{}",
            stratum.accepted, stratum.rejected, stratum.dropped
        ),
    );
    serial_writeln(usb, ar.as_str());
    serial_write(usb, "  reconnects = ");
    let mut rc: String<12> = String::new();
    let _ = core::fmt::Write::write_fmt(&mut rc, format_args!("{}", stratum.reconnects));
    serial_writeln(usb, rc.as_str());
    serial_write(usb, "  detail = ");
    if stratum.detail.is_empty() {
        serial_writeln(usb, "(none)");
    } else {
        serial_writeln(usb, stratum.detail.as_str());
    }
    serial_write(usb, "  job = ");
    if stratum.job_id.is_empty() {
        serial_writeln(usb, "(none)");
    } else {
        serial_writeln(usb, stratum.job_id.as_str());
    }
}

async fn resolve_pool_config<D: embedded_hal::delay::DelayNs>(
    usb: &mut Serial<'_>,
    display: &mut Display<'_, D>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    store: &mut ConfigStore<'_>,
    force_change: bool,
    wifi_token: &mut Option<WIFI<'static>>,
    boot: &Input<'_>,
) -> (PoolConfig, bool) {
    match store.load() {
        Ok(saved) => {
            serial_writeln(usb, "");
            serial_writeln(usb, "=== Saved credentials found ===");
            print_config_serial(usb, &saved);
            serial_writeln(
                usb,
                "Tap CONF or type 'change' within 8s to edit, or wait.",
            );

            let _ = display.draw_config_summary(&saved, true);

            let want_change = force_change
                || wait_for_change_or_touch(
                    usb,
                    touch,
                    touch_delay,
                    Duration::from_secs(SAVED_CONFIRM_SECS),
                )
                .await;

            if want_change {
                if force_change {
                    serial_writeln(usb, "BOOT held — password required to change credentials.");
                }
                if let Some(updated) = password_gated_change(
                    usb,
                    display,
                    touch,
                    touch_delay,
                    store,
                    &saved,
                    wifi_token,
                    boot,
                )
                .await
                {
                    return (updated, true);
                }
                serial_writeln(usb, "Keeping previously saved credentials.");
                return (saved, true);
            }
            (saved, true)
        }
        Err(_) => {
            serial_writeln(usb, "");
            // Default path: configure via Windows companion over USB (no on-device typing).
            // Hold BOOT at power-on to use classic touch/serial setup instead.
            if force_change {
                serial_writeln(
                    usb,
                    "BOOT held — on-device first-time setup (touch or serial).",
                );
                let mut cfg = collect_pool_config(
                    usb,
                    display,
                    touch,
                    touch_delay,
                    store,
                    wifi_token,
                    boot,
                )
                .await;
                cfg.touch_map = touch.map.id();
                match store.save(&cfg) {
                    Ok(()) => {
                        serial_writeln(usb, "Credentials saved to flash.");
                        (cfg, true)
                    }
                    Err(_) => {
                        serial_writeln(usb, "WARNING: flash save failed.");
                        (cfg, false)
                    }
                }
            } else {
                serial_writeln(usb, "No saved credentials — waiting for CYD Companion (USB).");
                serial_writeln(
                    usb,
                    "Hold BOOT at power-on for on-device setup instead.",
                );
                let mut cfg = PoolConfig::new();
                cfg.touch_map = touch.map.id();
                let _ = display.draw_waiting_companion();
                (cfg, false)
            }
        }
    }
}

async fn password_gated_change<D: embedded_hal::delay::DelayNs>(
    usb: &mut Serial<'_>,
    display: &mut Display<'_, D>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    store: &mut ConfigStore<'_>,
    current: &PoolConfig,
    wifi_token: &mut Option<WIFI<'static>>,
    boot: &Input<'_>,
) -> Option<PoolConfig> {
    serial_writeln(usb, "");
    serial_writeln(usb, "=== Change credentials (password required) ===");
    serial_writeln(usb, "Use on-screen keyboard or USB serial.");

    for attempt in 1..=MAX_PASSWORD_ATTEMPTS {
        serial_write(usb, "current password");
        if attempt > 1 {
            serial_write(usb, " (retry)");
        }
        serial_write(usb, ": ");
        let _ = usb.flush();

        let mut line: String<128> = String::new();
        read_field_touch_or_serial(
            usb,
            display,
            touch,
            touch_delay,
            SetupField::Password,
            &mut line,
            true,
            Some((attempt, MAX_PASSWORD_ATTEMPTS)),
            boot,
            "",
        )
        .await;

        match current.authorize(line.as_str()) {
            Ok(()) => {
                serial_writeln(usb, "  ok — enter new values");
                let mut cfg =
                    collect_pool_config(usb, display, touch, touch_delay, store, wifi_token, boot)
                        .await;
                cfg.touch_map = touch.map.id();
                match store.save(&cfg) {
                    Ok(()) => {
                        serial_writeln(usb, "Updated credentials saved to flash.");
                        return Some(cfg);
                    }
                    Err(_) => {
                        serial_writeln(usb, "WARNING: auth ok but flash save failed.");
                        return Some(cfg);
                    }
                }
            }
            Err(ConfigError::BadPassword) => {
                serial_writeln(usb, "  incorrect password");
            }
            Err(_) => serial_writeln(usb, "  auth error"),
        }
    }

    serial_writeln(usb, "Too many failed attempts — change cancelled.");
    None
}

async fn wait_for_change_or_touch(
    usb: &mut Serial<'_>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let mut line: String<32> = String::new();
    let mut byte = [0u8; 1];

    while Instant::now() < deadline {
        if let Some(p) = touch.poll_tap(touch_delay) {
            if matches!(hit_gui(p, false), Some(GuiHit::Tab(1) | GuiHit::ChangeBanner)) {
                return true;
            }
        }
        match usb.read(&mut byte) {
            Ok(0) | Err(_) => {
                Timer::after(Duration::from_millis(20)).await;
            }
            Ok(_) => match byte[0] {
                b'\n' | b'\r' => {
                    if !line.is_empty() {
                        let hit = is_change_command(line.as_str().trim());
                        line.clear();
                        if hit {
                            return true;
                        }
                    }
                }
                c if (32..127).contains(&c) => {
                    let _ = line.push(c as char);
                }
                _ => {}
            },
        }
    }
    false
}

/// Drain the UART RX FIFO until one full line is assembled (or the FIFO is empty).
///
/// Critical for the Windows companion: `mine_batch` can take hundreds of ms–seconds,
/// so reading only one byte per loop left `cmp status` sitting in the FIFO and the
/// host timed out after a few leftover TX bytes.
fn poll_command_line<const N: usize>(usb: &mut Serial<'_>, line: &mut String<N>) -> bool {
    let mut byte = [0u8; 1];
    for _ in 0..512 {
        match usb.read(&mut byte) {
            Ok(0) | Err(_) => return false,
            Ok(_) => match byte[0] {
                b'\n' | b'\r' => {
                    if !line.is_empty() {
                        return true;
                    }
                }
                c if (32..127).contains(&c) => {
                    let _ = line.push(c as char);
                }
                _ => {}
            },
        }
    }
    false
}

fn strip_cmp_prefix(cmd: &str) -> Option<&str> {
    let t = cmd.trim();
    if t.len() >= 3 && t.as_bytes()[..3].eq_ignore_ascii_case(b"cmp") {
        let rest = t[3..].trim_start();
        Some(if rest.is_empty() { "ping" } else { rest })
    } else {
        None
    }
}

/// True if BOOT stays low for `ms` continuously (intentional hold).
async fn boot_hold_ms(boot: &Input<'_>, ms: u64) -> bool {
    if !boot.is_low() {
        return false;
    }
    let start = Instant::now();
    while boot.is_low() {
        if start.elapsed() >= Duration::from_millis(ms) {
            return true;
        }
        Timer::after(Duration::from_millis(20)).await;
    }
    false
}

/// Result of handling a `cmp …` line while classic setup UI is on screen.
enum SetupCmp {
    /// Not a companion command.
    NotCmp,
    /// Answered ping/status/config (or partial set) — keep prompting.
    Continue,
    /// Companion finished setup; use this config and leave classic setup.
    Finished(PoolConfig),
}

fn find_cmp_start(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i].eq_ignore_ascii_case(&b'c')
            && b[i + 1].eq_ignore_ascii_case(&b'm')
            && b[i + 2].eq_ignore_ascii_case(&b'p')
            && (i + 3 == b.len() || b[i + 3].is_ascii_whitespace())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

async fn handle_cmp_during_setup(
    usb: &mut Serial<'_>,
    store: &mut ConfigStore<'_>,
    cfg: &mut PoolConfig,
    touch: &mut Touch,
    line: &str,
) -> SetupCmp {
    // Classic setup may prepend a default stratum value before USB bytes arrive.
    let line = match find_cmp_start(line) {
        Some(i) => &line[i..],
        None => return SetupCmp::NotCmp,
    };
    let Some(rest) = strip_cmp_prefix(line) else {
        return SetupCmp::NotCmp;
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("ping");
    let args = parts.next().unwrap_or("").trim();

    if eq_ignore_ascii_case(verb, "ping") {
        serial_writeln(usb, "CMP ok usb");
        return SetupCmp::Continue;
    }
    if eq_ignore_ascii_case(verb, "status") {
        let mut out: String<384> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "CMPSTATUS {{\"hashrate_hs\":0.00,\"shares\":0,\"accepted\":0,\"rejected\":0,\
\"dropped\":0,\"pool\":\"setup\",\"connected\":false,\"wifi\":\"off\",\"ip\":\"---\",\
\"address\":\"{}\",\"stratum\":\"{}\",\"difficulty\":0,\"uptime_secs\":0,\"cpu_mhz\":{},\
\"hash_focus\":{},\"nonce\":\"00000000\"}}",
                cfg.address.as_str(),
                cfg.stratum.as_str(),
                cfg.cpu_mhz,
                if cfg.hash_focus { "true" } else { "false" },
            ),
        );
        serial_writeln(usb, out.as_str());
        return SetupCmp::Continue;
    }
    if eq_ignore_ascii_case(verb, "config") {
        let mut out: String<384> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "CMPCONFIG {{\"worker\":\"{}\",\"stratum\":\"{}\",\"wifi_ssid\":\"{}\",\
\"wifi_password\":\"{}\",\"cpu_mhz\":{},\"hash_focus\":{},\"fw\":\"{}\",\"configured\":{}}}",
                cfg.address.as_str(),
                cfg.stratum.as_str(),
                cfg.wifi_ssid.as_str(),
                cfg.wifi_password_masked().as_str(),
                cfg.cpu_mhz,
                if cfg.hash_focus { "true" } else { "false" },
                env!("CARGO_PKG_VERSION"),
                if cfg.is_complete() { "true" } else { "false" },
            ),
        );
        serial_writeln(usb, out.as_str());
        return SetupCmp::Continue;
    }
    if eq_ignore_ascii_case(verb, "set")
        || eq_ignore_ascii_case(verb, "clock")
        || eq_ignore_ascii_case(verb, "reboot")
    {
        let mut upd = esp32_s3_scrypt_miner::web::parse_companion_body(args);
        if eq_ignore_ascii_case(verb, "reboot") {
            upd.reboot = true;
        }
        if pool_allows_setup_write(cfg, upd.auth.as_str()).is_err() {
            serial_writeln(usb, "CMPERR bad auth");
            return SetupCmp::Continue;
        }
        serial_writeln(usb, "CMPACK queued");
        apply_companion_update(usb, store, cfg, touch, upd, false, cfg.cpu_mhz).await;
        if cfg.is_complete() {
            return SetupCmp::Finished(cfg.clone());
        }
        return SetupCmp::Continue;
    }
    serial_writeln(usb, "CMPERR unknown (ping|status|config|set|clock|reboot)");
    SetupCmp::Continue
}

fn pool_allows_setup_write(cfg: &PoolConfig, auth: &str) -> Result<(), ()> {
    cfg.authorize_or_setup(auth).map_err(|_| ())
}

async fn handle_cmp_command(
    usb: &mut Serial<'_>,
    store: &mut ConfigStore<'_>,
    pool: &mut PoolConfig,
    touch: &mut Touch,
    rest: &str,
    stats: &esp32_s3_scrypt_miner::miner::MinerStats,
    hashrate_x100: u32,
    radio: &RadioStatus,
    stratum: &StratumStatus,
    running_mhz: u8,
    stratum_enabled: bool,
) {
    let mut parts = rest.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("ping");
    let args = parts.next().unwrap_or("").trim();

    if eq_ignore_ascii_case(verb, "ping") {
        serial_writeln(usb, "CMP ok usb");
        return;
    }
    if eq_ignore_ascii_case(verb, "status") {
        let ip = radio.ip_string();
        let pool_lab = if stratum.phase.is_connected() {
            "CONNECTED"
        } else {
            stratum.phase.label()
        };
        let address = json_escape(pool.address.as_str());
        let stratum_s = json_escape(pool.stratum.as_str());
        // Wide buffer: worker+stratum can each be 96 chars.
        let mut line: String<768> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut line,
            format_args!(
                "CMPSTATUS {{\"hashrate_hs\":{}.{:02},\"shares\":{},\"accepted\":{},\"rejected\":{},\
\"dropped\":{},\"pool\":\"{}\",\"connected\":{},\"wifi\":\"{}\",\"ip\":\"{}\",\"address\":\"{}\",\
\"stratum\":\"{}\",\"difficulty\":{},\"uptime_secs\":0,\"cpu_mhz\":{},\"hash_focus\":{},\
\"nonce\":\"{:08x}\"}}",
                hashrate_x100 / 100,
                hashrate_x100 % 100,
                stats.shares,
                stratum.accepted,
                stratum.rejected,
                stratum.dropped,
                pool_lab,
                if stratum.phase.is_connected() {
                    "true"
                } else {
                    "false"
                },
                radio.wifi.label(),
                ip.as_str(),
                address.as_str(),
                stratum_s.as_str(),
                stratum.difficulty,
                running_mhz,
                if pool.hash_focus { "true" } else { "false" },
                stats.nonce,
            ),
        );
        serial_writeln(usb, line.as_str());
        return;
    }
    if eq_ignore_ascii_case(verb, "config") {
        let worker = json_escape(pool.address.as_str());
        let stratum_s = json_escape(pool.stratum.as_str());
        let ssid = json_escape(pool.wifi_ssid.as_str());
        let mut line: String<768> = String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut line,
            format_args!(
                "CMPCONFIG {{\"worker\":\"{}\",\"stratum\":\"{}\",\"wifi_ssid\":\"{}\",\
\"wifi_password\":\"{}\",\"cpu_mhz\":{},\"hash_focus\":{},\"fw\":\"{}\",\"configured\":{}}}",
                worker.as_str(),
                stratum_s.as_str(),
                ssid.as_str(),
                pool.wifi_password_masked().as_str(),
                pool.cpu_mhz,
                if pool.hash_focus { "true" } else { "false" },
                env!("CARGO_PKG_VERSION"),
                if pool.is_complete() { "true" } else { "false" },
            ),
        );
        serial_writeln(usb, line.as_str());
        return;
    }
    if eq_ignore_ascii_case(verb, "set")
        || eq_ignore_ascii_case(verb, "clock")
        || eq_ignore_ascii_case(verb, "reboot")
    {
        let mut upd = esp32_s3_scrypt_miner::web::parse_companion_body(args);
        if eq_ignore_ascii_case(verb, "reboot") {
            upd.reboot = true;
        }
        if eq_ignore_ascii_case(verb, "clock") && upd.cpu_mhz.is_none() {
            serial_writeln(usb, "CMPERR cpu_mhz required");
            return;
        }
        if pool.authorize_or_setup(upd.auth.as_str()).is_err() {
            serial_writeln(usb, "CMPERR bad auth");
            return;
        }
        serial_writeln(usb, "CMPACK queued");
        apply_companion_update(
            usb,
            store,
            pool,
            touch,
            upd,
            stratum_enabled,
            running_mhz,
        )
        .await;
        return;
    }
    serial_writeln(usb, "CMPERR unknown (ping|status|config|set|clock|reboot)");
}

fn is_change_command(cmd: &str) -> bool {
    eq_ignore_ascii_case(cmd, "change")
        || eq_ignore_ascii_case(cmd, "edit")
        || eq_ignore_ascii_case(cmd, "update")
        || eq_ignore_ascii_case(cmd, "clear")
}

fn is_radio_command(cmd: &str) -> bool {
    eq_ignore_ascii_case(cmd, "radio")
        || eq_ignore_ascii_case(cmd, "wifi")
        || eq_ignore_ascii_case(cmd, "stratum")
        || eq_ignore_ascii_case(cmd, "pool")
}

fn is_touch_command(cmd: &str) -> bool {
    eq_ignore_ascii_case(cmd, "touch")
        || eq_ignore_ascii_case(cmd, "touchmap")
        || eq_ignore_ascii_case(cmd, "tmap")
}

fn eq_ignore_ascii_case(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

async fn collect_pool_config<D: embedded_hal::delay::DelayNs>(
    usb: &mut Serial<'_>,
    display: &mut Display<'_, D>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    store: &mut ConfigStore<'_>,
    wifi_token: &mut Option<WIFI<'static>>,
    boot: &Input<'_>,
) -> PoolConfig {
    let mut cfg = PoolConfig::new();
    let mut skip_wifi_password = false;

    serial_writeln(usb, "");
    serial_writeln(usb, "=== ESP32-2432S028 Scrypt Miner setup ===");
    serial_writeln(usb, "Tip: CYD Companion `cmp set …` also works here (preferred).");
    serial_writeln(usb, "Step 1: pick a WiFi network (required).");
    serial_writeln(usb, "Serial: number from scan list, or type the SSID + Enter.");
    serial_writeln(usb, "BOOT: select highlighted network · keyboard short=next, long=press.");
    serial_writeln(usb, "");

    for field in SetupField::ALL {
        if field == SetupField::WifiPassword && skip_wifi_password {
            continue;
        }
        if !field.allows_empty() && !cfg.get(field).is_empty() {
            continue;
        }

        if field == SetupField::WifiSsid {
            loop {
                match pick_wifi_ssid(usb, display, touch, touch_delay, store, &mut cfg, wifi_token, boot)
                    .await
                {
                    WifiPick::Network { ssid, open } => {
                        match cfg.set(SetupField::WifiSsid, ssid.as_str()) {
                            Ok(()) => {
                                serial_write(usb, "  ok (wifi_ssid=");
                                serial_write(usb, ssid.as_str());
                                serial_writeln(usb, ")");
                                skip_wifi_password = open;
                                if open {
                                    let _ = cfg.set(SetupField::WifiPassword, "");
                                    serial_writeln(usb, "  open network — no password");
                                }
                                break;
                            }
                            Err(e) => {
                                serial_write(usb, "  error: ");
                                serial_writeln(usb, config_error_msg(e));
                                serial_writeln(usb, " — WiFi is required, try again");
                            }
                        }
                    }
                    WifiPick::CompanionDone(done) => return done,
                }
            }
            continue;
        }

        loop {
            serial_write(usb, field.label());
            if field == SetupField::Stratum {
                serial_write(usb, " [");
                serial_write(usb, DEFAULT_STRATUM);
                serial_write(usb, "]");
            }
            serial_write(usb, ": ");
            let _ = usb.flush();

            let mut line: String<384> = String::new();
            let initial = if field == SetupField::Stratum {
                DEFAULT_STRATUM
            } else {
                ""
            };
            read_field_touch_or_serial(
                usb,
                display,
                touch,
                touch_delay,
                field,
                &mut line,
                field.is_secret(),
                None,
                boot,
                initial,
            )
            .await;

            match handle_cmp_during_setup(usb, store, &mut cfg, touch, line.as_str()).await {
                SetupCmp::NotCmp => {}
                SetupCmp::Continue => continue,
                SetupCmp::Finished(done) => return done,
            }

            let (target, value) =
                if let Ok((parsed_field, value)) = PoolConfig::parse_assignment(line.as_str()) {
                    (parsed_field, value)
                } else {
                    (field, line.as_str())
                };

            match cfg.set(target, value) {
                Ok(()) => {
                    serial_write(usb, "  ok (");
                    serial_write(usb, target.label());
                    serial_writeln(usb, ")");
                    if target == field {
                        break;
                    }
                    if !field.allows_empty() && !cfg.get(field).is_empty() {
                        break;
                    }
                }
                Err(e) => {
                    serial_write(usb, "  error: ");
                    serial_writeln(usb, config_error_msg(e));
                    serial_writeln(usb, " — try again");
                }
            }
        }
    }

    cfg
}

enum WifiPick {
    Network { ssid: String<32>, open: bool },
    CompanionDone(PoolConfig),
}

async fn pick_wifi_ssid<D: embedded_hal::delay::DelayNs>(
    usb: &mut Serial<'_>,
    display: &mut Display<'_, D>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    store: &mut ConfigStore<'_>,
    cfg: &mut PoolConfig,
    _wifi_token: &mut Option<WIFI<'static>>,
    boot: &Input<'_>,
) -> WifiPick {
    let mut networks: heapless::Vec<ScannedNetwork, 8> = heapless::Vec::new();
    let mut scroll = 0usize;
    let mut status: String<40> = String::new();
    let mut dirty = true;
    let mut byte = [0u8; 1];
    // Wide enough for companion `cmp set …` form bodies while scanning.
    let mut serial_buf: String<384> = String::new();
    let mut boot_was_down = boot.is_low();

    // Scan steals WIFI briefly; owned token stays for radio::start.
    let _ = status.push_str("scanning…");
    let _ = display.draw_wifi_scan(&[], 0, status.as_str());
    serial_writeln(usb, "Scanning WiFi…");
    match radio::scan_networks().await {
        Ok(list) => {
            networks = list;
            status.clear();
            if networks.is_empty() {
                let _ = status.push_str("no networks — tap scan or type");
            }
            print_scan_list(usb, &networks);
        }
        Err(()) => {
            status.clear();
            let _ = status.push_str("scan failed — tap scan or type");
            serial_writeln(usb, "WiFi scan failed.");
        }
    }

    loop {
        if dirty {
            let _ = display.draw_wifi_scan(&networks, scroll, status.as_str());
            dirty = false;
        }

        if let Some(p) = touch.poll_tap(touch_delay) {
            match hit_wifi_scan(p, scroll, networks.len()) {
                Some(WifiScanHit::Select(i)) => {
                    if let Some(n) = networks.get(i) {
                        let mut ssid: String<32> = String::new();
                        let _ = ssid.push_str(n.ssid.as_str());
                        return WifiPick::Network {
                            ssid,
                            open: n.open,
                        };
                    }
                }
                Some(WifiScanHit::ScrollUp) => {
                    scroll = scroll.saturating_sub(1);
                    dirty = true;
                }
                Some(WifiScanHit::ScrollDown) => {
                    let max_scroll = networks.len().saturating_sub(1);
                    if scroll < max_scroll {
                        scroll += 1;
                        dirty = true;
                    }
                }
                Some(WifiScanHit::Rescan) => {
                    status.clear();
                    let _ = status.push_str("scanning…");
                    let _ = display.draw_wifi_scan(&networks, scroll, status.as_str());
                    serial_writeln(usb, "Rescanning WiFi…");
                    match radio::scan_networks().await {
                        Ok(list) => {
                            networks = list;
                            scroll = 0;
                            status.clear();
                            if networks.is_empty() {
                                let _ = status.push_str("no networks — tap scan or type");
                            }
                            print_scan_list(usb, &networks);
                        }
                        Err(()) => {
                            status.clear();
                            let _ = status.push_str("scan failed — tap scan or type");
                            serial_writeln(usb, "WiFi scan failed.");
                        }
                    }
                    dirty = true;
                }
                Some(WifiScanHit::TypeManual) => {
                    let mut line: String<384> = String::new();
                    serial_write(usb, "wifi_ssid (type): ");
                    let _ = usb.flush();
                    read_field_touch_or_serial(
                        usb,
                        display,
                        touch,
                        touch_delay,
                        SetupField::WifiSsid,
                        &mut line,
                        false,
                        None,
                        boot,
                        "",
                    )
                    .await;
                    match handle_cmp_during_setup(usb, store, cfg, touch, line.as_str()).await {
                        SetupCmp::NotCmp => {}
                        SetupCmp::Continue => {
                            dirty = true;
                            continue;
                        }
                        SetupCmp::Finished(done) => return WifiPick::CompanionDone(done),
                    }
                    let trimmed = line.as_str().trim();
                    if trimmed.is_empty()
                        || trimmed == "-"
                        || eq_ignore_ascii_case(trimmed, "skip")
                    {
                        serial_writeln(usb, "  WiFi SSID required — pick again");
                        dirty = true;
                        continue;
                    }
                    let mut ssid: String<32> = String::new();
                    let _ = ssid.push_str(trimmed);
                    return WifiPick::Network {
                        ssid,
                        open: false,
                    };
                }
                None => {}
            }
            continue;
        }

        // BOOT short: select highlighted network (or cycle touch map if none).
        let boot_down = boot.is_low();
        if !boot_down && boot_was_down {
            if let Some(n) = networks.get(scroll) {
                let mut ssid: String<32> = String::new();
                let _ = ssid.push_str(n.ssid.as_str());
                serial_writeln(usb, "BOOT: selected network");
                return WifiPick::Network {
                    ssid,
                    open: n.open,
                };
            }
            touch.cycle_map();
            serial_write(usb, "Touch map → ");
            serial_writeln(usb, touch.map.label());
            dirty = true;
        }
        boot_was_down = boot_down;

        match usb.read(&mut byte) {
            Ok(0) | Err(_) => {
                Timer::after(Duration::from_millis(12)).await;
            }
            Ok(_) => {
                let c = byte[0];
                match c {
                    b'\n' | b'\r' => {
                        let trimmed = serial_buf.as_str().trim();
                        if trimmed.is_empty() {
                            serial_buf.clear();
                            continue;
                        }
                        match handle_cmp_during_setup(usb, store, cfg, touch, trimmed).await {
                            SetupCmp::NotCmp => {}
                            SetupCmp::Continue => {
                                serial_buf.clear();
                                dirty = true;
                                continue;
                            }
                            SetupCmp::Finished(done) => return WifiPick::CompanionDone(done),
                        }
                        if trimmed == "-" || eq_ignore_ascii_case(trimmed, "skip") {
                            serial_writeln(usb, "WiFi is required — enter a number or SSID.");
                            serial_buf.clear();
                            continue;
                        }
                        if let Ok(n) = trimmed.parse::<usize>() {
                            if (1..=networks.len()).contains(&n) {
                                let net = &networks[n - 1];
                                let mut ssid: String<32> = String::new();
                                let _ = ssid.push_str(net.ssid.as_str());
                                serial_buf.clear();
                                return WifiPick::Network {
                                    ssid,
                                    open: net.open,
                                };
                            }
                            serial_writeln(usb, "Invalid number — try again.");
                            serial_buf.clear();
                            continue;
                        }
                        // Treat as typed SSID.
                        let mut ssid: String<32> = String::new();
                        let _ = ssid.push_str(trimmed);
                        serial_buf.clear();
                        return WifiPick::Network {
                            ssid,
                            open: false,
                        };
                    }
                    0x08 | 0x7f => {
                        let _ = serial_buf.pop();
                        let _ = usb.write_all(b"\x08 \x08");
                    }
                    c if (32..127).contains(&c) => {
                        if serial_buf.push(c as char).is_ok() {
                            let _ = usb.write_all(&[c]);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn print_scan_list(usb: &mut Serial<'_>, networks: &[ScannedNetwork]) {
    if networks.is_empty() {
        serial_writeln(usb, "(no networks found)");
        return;
    }
    serial_writeln(usb, "Networks:");
    for (i, n) in networks.iter().enumerate() {
        let mut line: String<64> = String::new();
        let kind = if n.open { "open" } else { "lock" };
        let _ = core::fmt::Write::write_fmt(
            &mut line,
            format_args!("  {}. {}  {} {}dBm", i + 1, n.ssid, kind, n.rssi),
        );
        serial_writeln(usb, line.as_str());
    }
    serial_writeln(usb, "Enter number or SSID (WiFi required).");
}

/// Collect one field via on-screen keyboard and/or USB serial.
/// BOOT short = next key, BOOT long = activate key; finger-up taps also work.
async fn read_field_touch_or_serial<D: embedded_hal::delay::DelayNs, const N: usize>(
    usb: &mut Serial<'_>,
    display: &mut Display<'_, D>,
    touch: &mut Touch,
    touch_delay: &mut Delay,
    field: SetupField,
    line: &mut String<N>,
    secret: bool,
    auth: Option<(u8, u8)>,
    boot: &Input<'_>,
    initial: &str,
) {
    line.clear();
    let _ = line.push_str(initial);
    let mut kb = Keyboard::default();
    let mut dirty = true;
    let mut byte = [0u8; 1];
    let mut boot_was_down = boot.is_low();
    let mut boot_down_since: Option<Instant> = None;
    let mut last_cursor: Option<(u16, u16)> = None;
    let mut finger_was_down = false;

    loop {
        let held = touch.poll_point(touch_delay);
        let finger_down = held.is_some();
        let cursor = held.map(|p| (p.x, p.y));
        if cursor != last_cursor {
            last_cursor = cursor;
            dirty = true;
        }

        // Release-edge tap using last sampled point.
        if finger_was_down && !finger_down {
            if let Some(p) = touch.last_point() {
                if let Some(action) = kb.hit_test(p) {
                    if kb.apply(action, line) {
                        let _ = usb.write_all(b"\r\n");
                        return;
                    }
                    dirty = true;
                } else {
                    serial_writeln(usb, "  (tap missed — BOOT short=next key)");
                }
            }
        }
        finger_was_down = finger_down;

        if dirty {
            if let Some((attempt, max)) = auth {
                let _ = display.draw_auth_keyboard(attempt, max, line.as_str(), &kb);
            } else {
                let _ = display.draw_setup_keyboard(field, line.as_str(), &kb);
            }
            if let Some((x, y)) = cursor {
                let _ = display.draw_touch_cursor(x, y);
            }
            dirty = false;
        }

        let boot_down = boot.is_low();
        if boot_down && !boot_was_down {
            boot_down_since = Some(Instant::now());
        }
        if !boot_down && boot_was_down {
            let long = boot_down_since
                .take()
                .map(|t| t.elapsed() >= Duration::from_millis(BOOT_LONG_PRESS_MS))
                .unwrap_or(false);
            if long {
                if let Some(action) = kb.focused_action() {
                    serial_writeln(usb, "BOOT long: key");
                    if kb.apply(action, line) {
                        let _ = usb.write_all(b"\r\n");
                        return;
                    }
                    dirty = true;
                }
            } else {
                kb.focus_next();
                dirty = true;
            }
        }
        boot_was_down = boot_down;

        match usb.read(&mut byte) {
            Ok(0) | Err(_) => {
                Timer::after(Duration::from_millis(16)).await;
            }
            Ok(_) => {
                let c = byte[0];
                match c {
                    b'\n' | b'\r' => {
                        let _ = usb.write_all(b"\r\n");
                        return;
                    }
                    0x08 | 0x7f => {
                        if line.pop().is_some() {
                            let _ = usb.write_all(b"\x08 \x08");
                            dirty = true;
                        }
                    }
                    c if (32..127).contains(&c) => {
                        if line.push(c as char).is_ok() {
                            if secret {
                                let _ = usb.write_all(b"*");
                            } else {
                                let _ = usb.write_all(&[c]);
                            }
                            dirty = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn apply_companion_update(
    usb: &mut Serial<'_>,
    store: &mut ConfigStore<'_>,
    pool: &mut PoolConfig,
    touch: &mut Touch,
    upd: esp32_s3_scrypt_miner::web::CompanionUpdate,
    stratum_enabled: bool,
    running_mhz: u8,
) {
    if pool.authorize_or_setup(upd.auth.as_str()).is_err() {
        serial_writeln(usb, "companion: bad auth — ignored");
        return;
    }
    let was_incomplete = !pool.is_complete();
    serial_writeln(usb, "companion: applying settings…");
    let mut wifi_changed = false;
    let mut clock_changed = false;

    if let Some(s) = upd.stratum.as_ref() {
        let _ = pool.set(SetupField::Stratum, s.as_str());
    }
    if let Some(w) = upd.worker.as_ref() {
        let _ = pool.set(SetupField::Address, w.as_str());
    }
    if let Some(p) = upd.password.as_ref() {
        let _ = pool.set(SetupField::Password, p.as_str());
    }
    if let Some(s) = upd.wifi_ssid.as_ref() {
        match pool.set(SetupField::WifiSsid, s.as_str()) {
            Ok(()) => {
                wifi_changed = true;
                serial_writeln(usb, "companion: wifi SSID updated");
            }
            Err(_) => serial_writeln(usb, "companion: wifi SSID rejected"),
        }
    }
    if let Some(p) = upd.wifi_password.as_ref() {
        // Empty string is valid (open network).
        match pool.set(SetupField::WifiPassword, p.as_str()) {
            Ok(()) => {
                wifi_changed = true;
                if p.is_empty() {
                    serial_writeln(usb, "companion: wifi password cleared (open)");
                } else {
                    serial_writeln(usb, "companion: wifi password updated");
                }
            }
            Err(_) => serial_writeln(usb, "companion: wifi password rejected"),
        }
    }
    if let Some(m) = upd.touch_map {
        pool.touch_map = m;
        touch.map = TouchMap::from_id(m);
    }
    if let Some(mhz) = upd.cpu_mhz {
        let mhz = normalize_cpu_mhz(mhz);
        if mhz != pool.cpu_mhz || mhz != running_mhz {
            pool.cpu_mhz = mhz;
            clock_changed = true;
            stash_boot_cpu_mhz(mhz);
        }
    }
    if let Some(focus) = upd.hash_focus {
        pool.hash_focus = focus;
        serial_writeln(
            usb,
            if focus {
                "companion: hash-focus ON (slower LCD)"
            } else {
                "companion: hash-focus OFF"
            },
        );
    }

    let saved = match store.save(pool) {
        Ok(()) => {
            serial_writeln(usb, "companion: saved to flash");
            true
        }
        Err(_) => {
            if !pool.is_complete() {
                serial_writeln(
                    usb,
                    "companion: not complete yet — send WiFi + pool together",
                );
            } else {
                serial_writeln(usb, "companion: flash save failed");
            }
            false
        }
    };
    web::set_runtime_meta(running_mhz, pool.wifi_password_masked().as_str());

    if stratum_enabled && (upd.reconnect || upd.stratum.is_some() || upd.worker.is_some() || upd.password.is_some())
    {
        stratum::apply_pool_config(pool).await;
        serial_writeln(usb, "companion: stratum reloaded");
    }

    // Reboot only after a successful flash save (first setup needs WiFi bring-up).
    let need_reboot = saved
        && (clock_changed
            || wifi_changed
            || upd.reboot
            || (was_incomplete && pool.is_complete()));
    if need_reboot {
        serial_writeln(usb, "companion: rebooting to apply…");
        Timer::after(Duration::from_millis(200)).await;
        esp_hal::system::software_reset();
    }
}

fn config_error_msg(e: ConfigError) -> &'static str {
    match e {
        ConfigError::Empty => "value cannot be empty",
        ConfigError::TooLong => "value too long",
        ConfigError::InvalidChar => "invalid character",
        ConfigError::UnknownField => "unknown field",
        ConfigError::Corrupt => "corrupt value",
        ConfigError::BadPassword => "incorrect password",
    }
}

fn serial_write(usb: &mut Serial<'_>, text: &str) {
    let _ = usb.write_all(text.as_bytes());
    let _ = usb.flush();
}

fn serial_writeln(usb: &mut Serial<'_>, text: &str) {
    let _ = usb.write_all(text.as_bytes());
    let _ = usb.write_all(b"\r\n");
    let _ = usb.flush();
}

/// Escape a string for embedding in a compact CMP* JSON line.
fn json_escape(input: &str) -> String<128> {
    let mut out: String<128> = String::new();
    for ch in input.chars() {
        match ch {
            '"' => {
                if out.push_str("\\\"").is_err() {
                    break;
                }
            }
            '\\' => {
                if out.push_str("\\\\").is_err() {
                    break;
                }
            }
            c if c.is_ascii_control() => {}
            c => {
                if out.push(c).is_err() {
                    break;
                }
            }
        }
    }
    out
}

