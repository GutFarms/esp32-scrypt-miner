//! Desktop GUI for the scrypt miner (credentials + live stats).
//!
//! ```text
//! cargo run --no-default-features --features host-gui --bin host-gui --release
//! ```

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use eframe::{App, Frame, NativeOptions};
use esp32_s3_scrypt_miner::config::{PoolConfig, SetupField};
use esp32_s3_scrypt_miner::miner::{hash_to_hex, MinerStats, ScryptMiner, SCRYPT_LOG_N, SCRYPT_N};
use esp32_s3_scrypt_miner::persist::{self, HOST_CONFIG_PATH};

enum MinerCmd {
    Stop,
}

struct MinerUpdate {
    stats: MinerStats,
    last_share: Option<String>,
}

fn main() -> eframe::Result<()> {
    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([560.0, 420.0])
            .with_title("SCRYPT · Host Miner"),
        ..Default::default()
    };
    eframe::run_native(
        "SCRYPT · Host Miner",
        options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            Box::new(HostGuiApp::new())
        }),
    )
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = Color32::from_rgb(18, 16, 14);
    style.visuals.window_fill = Color32::from_rgb(24, 20, 18);
    style.visuals.override_text_color = Some(Color32::from_rgb(240, 232, 220));
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 34, 28);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(64, 48, 32);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(180, 90, 30);
    style.visuals.selection.bg_fill = Color32::from_rgb(200, 100, 35);
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    ctx.set_style(style);
}

struct HostGuiApp {
    address: String,
    password: String,
    stratum: String,
    wifi_ssid: String,
    wifi_password: String,
    difficulty: u8,
    current_password: String,
    status: String,
    mining: bool,
    stats: MinerStats,
    last_share: String,
    cmd_tx: Option<Sender<MinerCmd>>,
    update_rx: Option<Receiver<MinerUpdate>>,
}

impl HostGuiApp {
    fn new() -> Self {
        let mut app = Self {
            address: String::new(),
            password: String::new(),
            stratum: String::new(),
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            difficulty: 4,
            current_password: String::new(),
            status: format!("Ready · config file {HOST_CONFIG_PATH}"),
            mining: false,
            stats: MinerStats::default(),
            last_share: String::new(),
            cmd_tx: None,
            update_rx: None,
        };
        if let Ok(cfg) = persist::load() {
            app.apply_config(&cfg);
            app.status = "Loaded saved credentials".into();
        }
        app
    }

    fn apply_config(&mut self, cfg: &PoolConfig) {
        self.address = cfg.address.to_string();
        self.password = cfg.password.to_string();
        self.stratum = cfg.stratum.to_string();
        self.wifi_ssid = cfg.wifi_ssid.to_string();
        self.wifi_password = cfg.wifi_password.to_string();
    }

    fn to_config(&self) -> Result<PoolConfig, String> {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Address, &self.address)
            .map_err(|e| e.to_string())?;
        cfg.set(SetupField::Password, &self.password)
            .map_err(|e| e.to_string())?;
        cfg.set(SetupField::Stratum, &self.stratum)
            .map_err(|e| e.to_string())?;
        cfg.set(SetupField::WifiSsid, &self.wifi_ssid)
            .map_err(|e| e.to_string())?;
        cfg.set(SetupField::WifiPassword, &self.wifi_password)
            .map_err(|e| e.to_string())?;
        Ok(cfg)
    }

    fn save_config(&mut self) {
        match self.to_config() {
            Ok(cfg) => match persist::save(&cfg) {
                Ok(()) => self.status = format!("Saved to {HOST_CONFIG_PATH}"),
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            Err(e) => self.status = format!("Invalid config: {e}"),
        }
    }

    fn change_config(&mut self) {
        let Ok(saved) = persist::load() else {
            self.status = "No saved credentials to change".into();
            return;
        };
        if saved.authorize(&self.current_password).is_err() {
            self.status = "Incorrect current password".into();
            return;
        }
        match self.to_config() {
            Ok(cfg) => match persist::save(&cfg) {
                Ok(()) => {
                    self.current_password.clear();
                    self.status = "Credentials updated (password ok)".into();
                }
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            Err(e) => self.status = format!("Invalid config: {e}"),
        }
    }

    fn start_mining(&mut self) {
        if self.mining {
            return;
        }
        let cfg = match self.to_config() {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Cannot start: {e}");
                return;
            }
        };
        let _ = persist::save(&cfg);
        let difficulty = self.difficulty;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        self.cmd_tx = Some(cmd_tx);
        self.update_rx = Some(update_rx);
        self.mining = true;
        self.status = format!("Mining · {} · {}", cfg.address, cfg.stratum);

        thread::spawn(move || {
            let mut miner = ScryptMiner::new_demo(difficulty);
            let mut window_start = Instant::now();
            let mut window_hashes = 0u64;
            let mut last_share = None;
            loop {
                if matches!(cmd_rx.try_recv(), Ok(MinerCmd::Stop)) {
                    break;
                }
                let result = miner.mine_one();
                window_hashes += 1;
                if result.is_share {
                    let mut hex = heapless::String::<128>::new();
                    hash_to_hex(&result.hash, 12, &mut hex);
                    last_share = Some(format!("nonce {:08x}  {hex}", result.nonce));
                }
                if window_start.elapsed() >= Duration::from_millis(250) {
                    let mut stats = miner.stats();
                    let ms = window_start.elapsed().as_millis().max(1) as u64;
                    stats.hashrate_x100 = ((window_hashes * 100_000) / ms) as u32;
                    let _ = update_tx.send(MinerUpdate {
                        stats,
                        last_share: last_share.clone(),
                    });
                    window_start = Instant::now();
                    window_hashes = 0;
                }
            }
        });
    }

    fn stop_mining(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(MinerCmd::Stop);
        }
        self.update_rx = None;
        self.mining = false;
        self.status = "Stopped".into();
    }

    fn poll_updates(&mut self) {
        if let Some(rx) = &self.update_rx {
            while let Ok(upd) = rx.try_recv() {
                self.stats = upd.stats;
                if let Some(s) = upd.last_share {
                    self.last_share = s;
                }
            }
        }
    }
}

impl App for HostGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        self.poll_updates();
        if self.mining {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::TopBottomPanel::top("brand").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("SCRYPT")
                        .size(28.0)
                        .color(Color32::from_rgb(232, 120, 40))
                        .strong(),
                );
                ui.label(
                    RichText::new(format!("  host miner · N={SCRYPT_N} (2^{SCRYPT_LOG_N})"))
                        .size(14.0)
                        .color(Color32::from_rgb(160, 140, 120)),
                );
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&self.status).color(Color32::from_rgb(180, 160, 140)));
            ui.add_space(6.0);
        });

        egui::SidePanel::left("creds")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.heading("Credentials");
                ui.add_space(6.0);
                ui.label("Stratum");
                ui.text_edit_singleline(&mut self.stratum);
                ui.label("Worker");
                ui.text_edit_singleline(&mut self.address);
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                ui.add_space(8.0);
                ui.label("WiFi SSID (required)");
                ui.text_edit_singleline(&mut self.wifi_ssid);
                ui.label("WiFi password");
                ui.add(egui::TextEdit::singleline(&mut self.wifi_password).password(true));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Difficulty");
                    ui.add(egui::Slider::new(&mut self.difficulty, 2..=8));
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save_config();
                    }
                    if ui.button("Load").clicked() {
                        if let Ok(cfg) = persist::load() {
                            self.apply_config(&cfg);
                            self.status = "Loaded saved credentials".into();
                        } else {
                            self.status = "No saved file".into();
                        }
                    }
                });
                ui.separator();
                ui.label("Change saved (current password)");
                ui.add(egui::TextEdit::singleline(&mut self.current_password).password(true));
                if ui.button("Authorize & save changes").clicked() {
                    self.change_config();
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Miner");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let start = ui.add_enabled(!self.mining, egui::Button::new("Start mining"));
                if start.clicked() {
                    self.start_mining();
                }
                let stop = ui.add_enabled(self.mining, egui::Button::new("Stop"));
                if stop.clicked() {
                    self.stop_mining();
                }
            });

            ui.add_space(16.0);
            let rate = format!(
                "{}.{:02} H/s",
                self.stats.hashrate_x100 / 100,
                self.stats.hashrate_x100 % 100
            );
            ui.label(RichText::new(&rate).size(36.0).color(Color32::from_rgb(232, 120, 40)));

            ui.add_space(8.0);
            egui::Grid::new("stats")
                .num_columns(2)
                .spacing([20.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Status");
                    ui.label(if self.mining { "MINING" } else { "IDLE" });
                    ui.end_row();
                    ui.label("Nonce");
                    ui.monospace(format!("{:08x}", self.stats.nonce));
                    ui.end_row();
                    ui.label("Shares");
                    ui.label(self.stats.shares.to_string());
                    ui.end_row();
                    ui.label("Hashes");
                    ui.label(self.stats.hashes.to_string());
                    ui.end_row();
                    ui.label("Best");
                    let mut best = heapless::String::<128>::new();
                    hash_to_hex(&self.stats.best_hash, 12, &mut best);
                    ui.monospace(best.as_str());
                    ui.end_row();
                    ui.label("Last share");
                    ui.monospace(if self.last_share.is_empty() {
                        "—"
                    } else {
                        &self.last_share
                    });
                    ui.end_row();
                });

            ui.add_space(16.0);
            let frac = (self.stats.hashrate_x100 as f32 / 2000.0).clamp(0.0, 1.0);
            let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), 18.0));
            ui.painter()
                .rect_filled(rect, 4.0, Color32::from_rgb(40, 32, 26));
            let mut fill = rect;
            fill.set_width(rect.width() * frac);
            ui.painter()
                .rect_filled(fill, 4.0, Color32::from_rgb(220, 110, 40));
            ui.painter().rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0_f32, Color32::from_rgb(80, 60, 40)),
            );
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_mining();
    }
}
