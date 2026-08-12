//! ESP32-2432S028 (Cheap Yellow Display) GUI — ILI9341 SPI, 320×240 landscape.

use core::fmt::Write as _;

use embedded_graphics::Drawable;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_6X12, FONT_8X13_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::Primitive;
use embedded_graphics::primitives::{Line, PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::Text;
use embedded_hal::delay::DelayNs;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::Blocking;
use heapless::String;
use mipidsi::interface::SpiInterface;
use mipidsi::models::ILI9341Rgb565;
use mipidsi::options::{ColorOrder, Orientation, Rotation};
use mipidsi::{Builder, Display as MipiDisplay, NoResetPin};
use static_cell::StaticCell;

use crate::config::{PoolConfig, SetupField};
use crate::gui::{GuiScreen, GuiState, MenuItem};
use crate::keyboard::{
    Keyboard, WIFI_SCAN_ROW0_Y, WIFI_SCAN_ROW_H, WIFI_SCAN_VISIBLE,
};
use crate::miner::{MinerStats, SCRYPT_LOG_N, SCRYPT_N};
use crate::radio::{RadioStatus, ScannedNetwork, WifiPhase};
use crate::stratum::{StratumPhase, StratumStatus};

/// Landscape resolution after Deg90 rotation of the native 240×320 panel.
pub const DISPLAY_WIDTH: u16 = 320;
pub const DISPLAY_HEIGHT: u16 = 240;

// Match CYD Companion: deep blue-black + lime (8-bit → Rgb565 channel widths).
const LIME: Rgb565 = Rgb565::new(22, 60, 11); // (180, 240, 90)
const LIME_DIM: Rgb565 = Rgb565::new(15, 45, 8); // (120, 180, 70)
const BUBBLE: Rgb565 = Rgb565::new(7, 21, 14); // (56, 84, 118)
const BUBBLE_HI: Rgb565 = Rgb565::new(13, 42, 27); // (110, 168, 220)
const TEXT: Rgb565 = Rgb565::new(28, 59, 31); // (228, 238, 248)
const TEXT_MUTED: Rgb565 = Rgb565::new(16, 37, 21); // (130, 150, 170)
const ERR_SOFT: Rgb565 = Rgb565::new(25, 20, 12); // (200, 100, 100)

const BRAND: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_10X20, LIME);
const LABEL: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_6X12, TEXT_MUTED);
const VALUE: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_10X20, TEXT);
const VALUE_SM: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_8X13_BOLD, TEXT);
const OK: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_10X20, LIME);
const MUTED: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_6X12, TEXT_MUTED);
const KEY_TXT: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_6X10, TEXT);
const KEY_TXT_DIM: MonoTextStyle<'_, Rgb565> = MonoTextStyle::new(&FONT_6X10, BUBBLE_HI);

const ACCENT: Rgb565 = LIME_DIM;
const ACCENT_HOT: Rgb565 = LIME;
const PANEL: Rgb565 = Rgb565::new(2, 6, 4); // (18, 26, 36)
const PANEL_HI: Rgb565 = Rgb565::new(3, 10, 7); // (28, 40, 56)
const BAR_BG: Rgb565 = Rgb565::new(5, 14, 10); // (40, 56, 80)
const BAR_FG: Rgb565 = LIME;
const SELECT: Rgb565 = Rgb565::new(8, 35, 11); // (70, 140, 90)
const KEY_BG: Rgb565 = BUBBLE;
const KEY_BG_HOT: Rgb565 = BUBBLE_HI;
const KEY_OK: Rgb565 = Rgb565::new(4, 12, 3); // (32, 48, 28)
const BG_DEEP: Rgb565 = Rgb565::new(1, 3, 2); // (8, 12, 16)
const CORNER: Size = Size::new(10, 10);

type SpiBus<'a> = Spi<'a, Blocking>;
type SpiDev<'a> = ExclusiveDevice<SpiBus<'a>, Output<'a>, Delay>;
type SpiDi<'a> = SpiInterface<'a, SpiDev<'a>, Output<'a>>;
type MipiDisplayWrapper<'a> = MipiDisplay<SpiDi<'a>, ILI9341Rgb565, NoResetPin>;

pub struct Display<'a, D: DelayNs> {
    display: MipiDisplayWrapper<'a>,
    backlight: Output<'a>,
    last_screen: Option<GuiScreen>,
    /// Animation phase 0..255 for subtle pulse.
    pub tick: u8,
    _delay: core::marker::PhantomData<D>,
}

/// CYD TFT pins + SPI2 (HSPI).
pub struct DisplayPeripherals {
    pub spi: esp_hal::peripherals::SPI2<'static>,
    pub sclk: AnyPin<'static>,
    pub mosi: AnyPin<'static>,
    pub miso: AnyPin<'static>,
    pub cs: AnyPin<'static>,
    pub dc: AnyPin<'static>,
    pub backlight: AnyPin<'static>,
}

impl<'a, D: DelayNs> Display<'a, D> {
    fn draw_text(
        &mut self,
        text: &str,
        position: Point,
        style: MonoTextStyle<'_, Rgb565>,
    ) -> Result<(), Error> {
        Text::new(text, position, style)
            .draw(&mut self.display)
            .map(|_| ())
            .map_err(|_| Error::DisplayInterface("text"))
    }

    pub fn new(p: DisplayPeripherals, mut delay: D) -> Result<Self, Error> {
        let backlight = Output::new(p.backlight, Level::High, OutputConfig::default());
        let dc = Output::new(p.dc, Level::Low, OutputConfig::default());
        let cs = Output::new(p.cs, Level::High, OutputConfig::default());

        let spi = Spi::new(
            p.spi,
            SpiConfig::default()
                .with_frequency(Rate::from_mhz(40))
                .with_mode(SpiMode::_0),
        )
        .map_err(|_| Error::InitError)?
        .with_sck(p.sclk)
        .with_mosi(p.mosi)
        .with_miso(p.miso);

        let spi_dev = ExclusiveDevice::new(spi, cs, Delay::new());

        static SPI_BUF: StaticCell<[u8; 512]> = StaticCell::new();
        let buf = SPI_BUF.init([0u8; 512]);
        let di = SpiInterface::new(spi_dev, dc, buf);

        let display = Builder::new(ILI9341Rgb565, di)
            .display_size(240, 320)
            .orientation(Orientation::new().rotate(Rotation::Deg90))
            .color_order(ColorOrder::Bgr)
            .init(&mut delay)
            .map_err(|_| Error::InitError)?;

        let _ = delay;
        Ok(Self {
            display,
            backlight,
            last_screen: None,
            tick: 0,
            _delay: core::marker::PhantomData,
        })
    }

    pub fn bump_tick(&mut self) {
        self.tick = self.tick.wrapping_add(3);
    }

    fn wake_clear(&mut self) -> Result<(), Error> {
        self.backlight.set_high();
        self.display
            .clear(BG_DEEP)
            .map_err(|_| Error::DisplayInterface("clear"))?;
        Ok(())
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: Rgb565) -> Result<(), Error> {
        Rectangle::new(Point::new(x, y), Size::new(w, h))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(&mut self.display)
            .map(|_| ())
            .map_err(|_| Error::DisplayInterface("rect"))
    }

    fn round_panel(&mut self, x: i32, y: i32, w: u32, h: u32, color: Rgb565) -> Result<(), Error> {
        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, y), Size::new(w, h)),
            CORNER,
        )
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(&mut self.display)
        .map(|_| ())
        .map_err(|_| Error::DisplayInterface("panel"))
    }

    fn accent_stripe(&mut self) -> Result<(), Error> {
        // Pulsing lime top edge (companion CTA energy)
        let hot = self.tick < 128;
        let c = if hot { ACCENT_HOT } else { ACCENT };
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 3, c)?;
        // Cool blue side rail
        self.fill_rect(0, 3, 3, DISPLAY_HEIGHT as u32 - 3, BUBBLE_HI)?;
        Ok(())
    }

    fn header_bar(&mut self, tab: &str) -> Result<(), Error> {
        self.header_bar_wifi(tab, WifiPhase::Disabled, None)
    }

    fn header_bar_wifi(
        &mut self,
        tab: &str,
        wifi: WifiPhase,
        ip: Option<[u8; 4]>,
    ) -> Result<(), Error> {
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 28, PANEL)?;
        self.accent_stripe()?;
        self.draw_text("SCRYPT", Point::new(10, 20), BRAND)?;
        // Tab title stays compact mid-right; WiFi occupies the far corner.
        let _ = tab;
        self.draw_wifi_corner(wifi, ip)?;
        Ok(())
    }

    /// Top-right WiFi connection chip (IP when up, else short phase).
    fn draw_wifi_corner(&mut self, wifi: WifiPhase, ip: Option<[u8; 4]>) -> Result<(), Error> {
        // Clear prior text in the corner (header refresh without full clear).
        self.fill_rect(168, 8, 152, 18, PANEL)?;

        let mut label: String<20> = String::new();
        let style = match (wifi, ip) {
            (WifiPhase::Connected, Some([a, b, c, d])) => {
                let _ = write!(label, "{a}.{b}.{c}.{d}");
                OK
            }
            (WifiPhase::Connected, None) => {
                let _ = label.push_str("WiFi up");
                OK
            }
            (WifiPhase::Connecting, _) | (WifiPhase::Starting, _) => {
                let _ = label.push_str("WiFi…");
                KEY_TXT_DIM
            }
            (WifiPhase::Failed, _) => {
                let _ = label.push_str("WiFi fail");
                MonoTextStyle::new(&FONT_6X12, ERR_SOFT)
            }
            (WifiPhase::Disconnected, _) => {
                let _ = label.push_str("WiFi down");
                KEY_TXT_DIM
            }
            (WifiPhase::Disabled, _) => {
                let _ = label.push_str("WiFi off");
                MUTED
            }
        };

        // Right-align roughly within the 320px header.
        let w = (label.len() as i32) * 6;
        let x = (DISPLAY_WIDTH as i32 - 6 - w).max(170);
        self.draw_text(label.as_str(), Point::new(x, 20), style)?;
        Ok(())
    }

    fn tab_strip(&mut self, active: GuiScreen) -> Result<(), Error> {
        self.fill_rect(0, 28, DISPLAY_WIDTH as u32, 26, PANEL_HI)?;
        for (i, screen) in GuiScreen::ALL.iter().enumerate() {
            let x = 4 + i as i32 * 80;
            let selected = *screen == active;
            let bg = if selected { ACCENT_HOT } else { BUBBLE };
            self.round_panel(x, 30, 72, 20, bg)?;
            let style = if selected {
                MonoTextStyle::new(&FONT_6X12, BG_DEEP)
            } else {
                MUTED
            };
            self.draw_text(screen.title(), Point::new(x + 18, 44), style)?;
        }
        Ok(())
    }

    fn footer_hint(&mut self, text: &str) -> Result<(), Error> {
        self.fill_rect(0, 226, DISPLAY_WIDTH as u32, 14, PANEL)?;
        self.draw_text(text, Point::new(8, 236), MUTED)?;
        Ok(())
    }

    /// Bottom-middle active hashrate (all main GUI tabs).
    pub fn draw_hashrate_footer(&mut self, hashrate_x100: u32) -> Result<(), Error> {
        self.fill_rect(0, 226, DISPLAY_WIDTH as u32, 14, PANEL)?;
        let mut rate: String<24> = String::new();
        let _ = write!(
            rate,
            "{}.{:02} H/s",
            hashrate_x100 / 100,
            hashrate_x100 % 100
        );
        // FONT_8X13_BOLD ≈ 8px/glyph; center in 320px.
        let w = (rate.len() as i32) * 8;
        let x = ((DISPLAY_WIDTH as i32) - w) / 2;
        self.draw_text(rate.as_str(), Point::new(x.max(4), 236), VALUE_SM)?;
        Ok(())
    }

    pub fn draw_splash(&mut self) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        // Lime flare bands (companion CTA)
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 8, ACCENT_HOT)?;
        self.fill_rect(0, 8, DISPLAY_WIDTH as u32, 4, BUBBLE_HI)?;
        self.fill_rect(0, 200, DISPLAY_WIDTH as u32, 40, PANEL)?;
        self.round_panel(40, 70, 240, 90, PANEL_HI)?;
        self.draw_text("SCRYPT", Point::new(110, 100), BRAND)?;
        let mut line: String<48> = String::new();
        let _ = write!(line, "ESP32-CYD  N={}  USB companion", SCRYPT_N);
        self.draw_text(&line, Point::new(40, 130), LABEL)?;
        self.draw_text("warming up…", Point::new(118, 160), MUTED)?;
        self.draw_text("setup via CYD Companion", Point::new(70, 220), KEY_TXT_DIM)?;
        Ok(())
    }

    /// Shown when flash has no credentials — configure from the Windows app.
    pub fn draw_waiting_companion(&mut self) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 8, ACCENT_HOT)?;
        self.fill_rect(0, 8, DISPLAY_WIDTH as u32, 4, BUBBLE_HI)?;
        self.header_bar("SETUP")?;
        self.round_panel(16, 56, 288, 140, PANEL_HI)?;
        self.draw_text("Waiting for companion", Point::new(56, 88), BRAND)?;
        self.draw_text("1. Plug USB (CH340)", Point::new(40, 120), VALUE_SM)?;
        self.draw_text("2. Open CYD Companion", Point::new(40, 144), VALUE_SM)?;
        self.draw_text("3. Setup → Save & reboot", Point::new(40, 168), VALUE_SM)?;
        self.footer_hint("hold BOOT at power-on for on-device setup")?;
        Ok(())
    }

    /// WiFi scan picker (step 1 of setup).
    pub fn draw_wifi_scan(
        &mut self,
        networks: &[ScannedNetwork],
        scroll: usize,
        status: &str,
    ) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.header_bar("WIFI")?;
        self.draw_text("1/5  pick a network", Point::new(12, 42), LABEL)?;

        if networks.is_empty() {
            self.round_panel(8, 56, 304, 130, PANEL)?;
            self.draw_text(status, Point::new(24, 110), VALUE_SM)?;
        } else {
            let visible = WIFI_SCAN_VISIBLE.min(networks.len().saturating_sub(scroll));
            for row in 0..visible {
                let idx = scroll + row;
                let n = &networks[idx];
                let y = WIFI_SCAN_ROW0_Y + row as i32 * WIFI_SCAN_ROW_H;
                self.round_panel(8, y, 304, (WIFI_SCAN_ROW_H - 4) as u32, PANEL_HI)?;
                let name = PoolConfig::ellipsize(n.ssid.as_str(), 22);
                self.draw_text(name.as_str(), Point::new(16, y + 16), VALUE_SM)?;
                let mut meta: String<16> = String::new();
                let lock = if n.open { "open" } else { "lock" };
                let _ = write!(meta, "{lock} {:>3}", n.rssi);
                self.draw_text(meta.as_str(), Point::new(230, y + 16), MUTED)?;
            }
            if !status.is_empty() {
                self.draw_text(status, Point::new(12, 196), MUTED)?;
            }
        }

        // Footer actions (WiFi required — no skip)
        self.round_panel(8, 200, 32, 22, KEY_BG)?;
        self.draw_text("^", Point::new(18, 214), KEY_TXT)?;
        self.round_panel(48, 200, 32, 22, KEY_BG)?;
        self.draw_text("v", Point::new(58, 214), KEY_TXT)?;
        self.round_panel(88, 200, 100, 22, KEY_BG_HOT)?;
        self.draw_text("scan", Point::new(120, 214), KEY_TXT)?;
        self.round_panel(196, 200, 116, 22, ACCENT)?;
        self.draw_text("type", Point::new(236, 214), KEY_TXT)?;
        Ok(())
    }

    /// Waiting for association / DHCP.
    pub fn draw_connecting(&mut self, ssid: &str) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.header_bar("WIFI")?;
        self.round_panel(20, 70, 280, 100, PANEL)?;
        self.draw_text("connecting…", Point::new(100, 100), VALUE_SM)?;
        let ss = PoolConfig::ellipsize(ssid, 28);
        self.draw_text(ss.as_str(), Point::new(40, 130), MUTED)?;
        self.footer_hint("waiting for DHCP IP")?;
        Ok(())
    }

    /// Full-screen IP after DHCP succeeds.
    pub fn draw_online(&mut self, ssid: &str, ip: [u8; 4]) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 8, LIME)?;
        self.fill_rect(0, 8, DISPLAY_WIDTH as u32, 3, BUBBLE_HI)?;
        self.header_bar("ONLINE")?;
        self.round_panel(20, 56, 280, 140, PANEL_HI)?;
        self.draw_text("connected", Point::new(100, 80), LABEL)?;
        let mut ip_s: String<20> = String::new();
        let _ = write!(ip_s, "{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        self.draw_text(ip_s.as_str(), Point::new(70, 120), BRAND)?;
        let mut url: String<40> = String::new();
        let _ = write!(url, "http://{}/", ip_s.as_str());
        self.draw_text(url.as_str(), Point::new(60, 150), VALUE_SM)?;
        let ss = PoolConfig::ellipsize(ssid, 28);
        self.draw_text(ss.as_str(), Point::new(40, 176), MUTED)?;
        self.footer_hint("tap or wait · web UI on port 80")?;
        Ok(())
    }

    pub fn draw_setup_keyboard(
        &mut self,
        field: SetupField,
        typed: &str,
        kb: &Keyboard,
    ) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.header_bar("SETUP")?;

        let step_n = field.step_number();
        let steps = SetupField::SETUP_STEPS;
        for i in 1..=steps {
            let x = 12 + (i as i32 - 1) * 18;
            let color = if i < step_n {
                ACCENT
            } else if i == step_n {
                ACCENT_HOT
            } else {
                BAR_BG
            };
            self.round_panel(x, 34, 14, 8, color)?;
        }

        let mut step: String<40> = String::new();
        let _ = write!(step, "{}/{} {}", step_n, steps, field.label());
        self.draw_text(&step, Point::new(130, 42), LABEL)?;

        // Value field
        self.round_panel(6, 48, 308, 42, PANEL_HI)?;
        self.draw_text(field.prompt(), Point::new(12, 60), MUTED)?;
        let shown: String<96> = if field.is_secret() && !typed.is_empty() {
            let n = typed.len().min(24);
            let mut stars: String<96> = String::new();
            for _ in 0..n {
                let _ = stars.push('*');
            }
            stars
        } else if typed.is_empty() {
            let mut s: String<96> = String::new();
            let _ = s.push_str("tap / BOOT=next · long BOOT=OK");
            s
        } else {
            PoolConfig::ellipsize(typed, 34)
        };
        let style = if typed.is_empty() { MUTED } else { VALUE_SM };
        self.draw_text(shown.as_str(), Point::new(12, 78), style)?;

        self.draw_keyboard(kb)?;
        Ok(())
    }

    pub fn draw_keyboard(&mut self, kb: &Keyboard) -> Result<(), Error> {
        self.fill_rect(0, kb.origin_y - 4, DISPLAY_WIDTH as u32, 240 - kb.origin_y as u32 + 4, PANEL)?;
        for (i, key) in kb.keys().into_iter().enumerate() {
            let focused = i == kb.focus;
            let bg = if focused {
                ACCENT_HOT
            } else {
                match key.action {
                    crate::keyboard::KeyAction::Enter => KEY_OK,
                    crate::keyboard::KeyAction::Shift | crate::keyboard::KeyAction::Symbols => {
                        KEY_BG_HOT
                    }
                    crate::keyboard::KeyAction::Skip => ACCENT,
                    _ => KEY_BG,
                }
            };
            self.round_panel(key.x, key.y, key.w, key.h, bg)?;
            let tx = key.x + 4;
            let ty = key.y + (key.h as i32 / 2) + 3;
            let style = match key.action {
                crate::keyboard::KeyAction::Enter => OK,
                crate::keyboard::KeyAction::Skip => MonoTextStyle::new(&FONT_6X10, BG_DEEP),
                _ if focused => MonoTextStyle::new(&FONT_6X10, BG_DEEP),
                _ => KEY_TXT,
            };
            self.draw_text(key.label, Point::new(tx, ty), style)?;
        }
        Ok(())
    }

    /// Overlay crosshair for live touch feedback (call after a full screen draw).
    pub fn draw_touch_cursor(&mut self, x: u16, y: u16) -> Result<(), Error> {
        let px = i32::from(x);
        let py = i32::from(y);
        let style = PrimitiveStyle::with_stroke(LIME, 1);
        Line::new(Point::new(px.saturating_sub(10), py), Point::new(px + 10, py))
            .into_styled(style)
            .draw(&mut self.display)
            .map_err(|_| Error::DisplayInterface("cursor"))?;
        Line::new(Point::new(px, py.saturating_sub(10)), Point::new(px, py + 10))
            .into_styled(style)
            .draw(&mut self.display)
            .map_err(|_| Error::DisplayInterface("cursor"))?;
        Ok(())
    }

    pub fn draw_config_summary(&mut self, cfg: &PoolConfig, from_flash: bool) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.header_bar(if from_flash { "SAVED" } else { "READY" })?;
        self.tab_strip(GuiScreen::Config)?;

        self.round_panel(8, 60, 304, 150, PANEL)?;
        // Top→bottom matches setup: wifi → stratum → worker → password
        let wifi = if cfg.wifi_enabled() {
            PoolConfig::ellipsize(cfg.wifi_ssid.as_str(), 22)
        } else {
            PoolConfig::ellipsize("(wifi off)", 22)
        };
        self.draw_row(78, "wifi", wifi.as_str())?;
        self.draw_row(104, "stratum", &PoolConfig::ellipsize(cfg.stratum.as_str(), 26))?;
        self.draw_row(130, "worker", &PoolConfig::ellipsize(cfg.address.as_str(), 26))?;
        self.draw_row(156, "password", cfg.password_masked().as_str())?;

        let hint = if from_flash {
            "tap tabs · long BOOT=menu · serial: change"
        } else {
            "saved to flash — tap glass to explore"
        };
        self.footer_hint(hint)?;
        Ok(())
    }

    fn draw_row(&mut self, y: i32, label: &str, value: &str) -> Result<(), Error> {
        self.draw_text(label, Point::new(18, y), LABEL)?;
        self.draw_text(value, Point::new(90, y), VALUE_SM)?;
        Ok(())
    }

    pub fn draw_gui(
        &mut self,
        gui: &GuiState,
        stats: &MinerStats,
        cfg: &PoolConfig,
        radio: &RadioStatus,
        stratum: &StratumStatus,
        mining: bool,
    ) -> Result<(), Error> {
        self.bump_tick();
        let screen_changed = self.last_screen != Some(gui.screen);
        if screen_changed {
            self.wake_clear()?;
            self.header_bar_wifi(gui.screen.title(), radio.wifi, radio.ip)?;
            self.tab_strip(gui.screen)?;
            self.last_screen = Some(gui.screen);
        } else {
            // Refresh accent pulse + live WiFi corner without full clear
            let _ = self.accent_stripe();
            let _ = self.draw_wifi_corner(radio.wifi, radio.ip);
        }

        match gui.screen {
            GuiScreen::Mining => {
                self.draw_mining_body(stats, cfg, radio, stratum, mining, screen_changed)?
            }
            GuiScreen::Config => {
                if screen_changed {
                    self.draw_config_body(cfg)?;
                }
            }
            GuiScreen::Radio => self.draw_radio_body(cfg, radio, stratum, screen_changed)?,
            GuiScreen::Menu => self.draw_menu_body(gui.menu)?,
        }
        // Always last so H/s stays visible bottom-center on every tab.
        self.draw_hashrate_footer(stats.hashrate_x100)?;
        Ok(())
    }

    fn draw_mining_body(
        &mut self,
        stats: &MinerStats,
        cfg: &PoolConfig,
        radio: &RadioStatus,
        stratum: &StratumStatus,
        mining: bool,
        full: bool,
    ) -> Result<(), Error> {
        let connected = stratum.phase.is_connected();
        let active = mining && (connected || stratum.phase == StratumPhase::Disabled);

        if full {
            self.round_panel(8, 60, 200, 100, PANEL)?;
            self.round_panel(216, 60, 96, 100, PANEL)?;
            self.round_panel(8, 168, 304, 52, PANEL)?;
            let hint = if connected {
                "pool connected · live H/s"
            } else if radio.ip.is_some() {
                "wifi up · waiting for pool"
            } else {
                "tap tabs · BOOT short=next"
            };
            self.footer_hint(hint)?;
        }

        // Primary: active hashes/sec
        self.fill_rect(16, 68, 184, 40, PANEL)?;
        self.draw_text("H/s active", Point::new(16, 76), LABEL)?;
        let mut rate: String<24> = String::new();
        let _ = write!(
            rate,
            "{}.{:02}",
            stats.hashrate_x100 / 100,
            stats.hashrate_x100 % 100
        );
        let rate_style = if connected { BRAND } else { VALUE };
        self.draw_text(&rate, Point::new(16, 104), rate_style)?;

        let pct = core::cmp::min(100u32, stats.hashrate_x100 / 20);
        self.fill_rect(16, 130, 184, 12, BAR_BG)?;
        let pulse = if active {
            4 + (self.tick as u32 % 12)
        } else {
            0
        };
        let w = (184 * pct / 100).max(pulse);
        if w > 0 {
            let fg = if connected {
                if self.tick < 128 {
                    LIME_DIM
                } else {
                    LIME
                }
            } else if self.tick < 128 {
                BUBBLE_HI
            } else {
                BAR_FG
            };
            self.fill_rect(16, 130, w.min(184), 12, fg)?;
        }

        // Connection chip from stratum phase
        let chip = if connected {
            KEY_OK
        } else if stratum.phase == StratumPhase::Error {
            ACCENT
        } else {
            PANEL_HI
        };
        self.round_panel(224, 70, 80, 36, chip)?;
        let st = stratum.phase.chip();
        let st_style = if connected {
            OK
        } else if stratum.phase == StratumPhase::Error {
            KEY_TXT_DIM
        } else {
            LABEL
        };
        // Center-ish short labels in the chip
        let st_x = if st.len() >= 4 { 236 } else { 244 };
        self.draw_text(st, Point::new(st_x, 94), st_style)?;

        let mut shares: String<20> = String::new();
        let _ = write!(
            shares,
            "a{}/r{}",
            stratum.accepted, stratum.rejected
        );
        self.draw_text(&shares, Point::new(228, 124), VALUE_SM)?;

        // Activity dots when hashing
        for i in 0..4u8 {
            let on = active && ((self.tick.wrapping_add(i.wrapping_mul(40))) > 120);
            let c = if on {
                if connected {
                    LIME
                } else {
                    BUBBLE_HI
                }
            } else {
                BAR_BG
            };
            self.fill_rect(236 + i as i32 * 14, 146, 8, 8, c)?;
        }

        // Pool / connection detail (always visible — not replaced by IP)
        self.fill_rect(16, 174, 288, 40, PANEL)?;
        let conn = if connected {
            "CONNECTED"
        } else {
            match stratum.phase {
                StratumPhase::Disabled => "local demo",
                StratumPhase::WaitingWifi => "wait wifi",
                StratumPhase::Resolving => "resolving",
                StratumPhase::Connecting => "connecting",
                StratumPhase::Subscribing => "subscribe",
                StratumPhase::Authorizing => "authorize",
                StratumPhase::Error => "pool error",
                StratumPhase::Idle | StratumPhase::Mining => "CONNECTED",
            }
        };
        let mut line1: String<48> = String::new();
        let _ = write!(line1, "{conn}  d{}", stratum.difficulty);
        self.draw_text(
            line1.as_str(),
            Point::new(16, 188),
            if connected { OK } else { LABEL },
        )?;

        let mut line2: String<56> = String::new();
        if let Some([a, b, c, d]) = radio.ip {
            let _ = write!(line2, "{a}.{b}.{c}.{d}");
            if !stratum.job_id.is_empty() {
                let job = PoolConfig::ellipsize(stratum.job_id.as_str(), 12);
                let _ = write!(line2, "  {}", job.as_str());
            }
        } else {
            let _ = write!(line2, "nonce {:08x}", stats.nonce);
        }
        self.draw_text(line2.as_str(), Point::new(16, 206), MUTED)?;

        let _ = (SCRYPT_LOG_N, cfg, mining);
        Ok(())
    }

    /// Splash when stratum authorizes / starts mining.
    pub fn draw_pool_connected(
        &mut self,
        stratum_host: &str,
        hashrate_x100: u32,
    ) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.fill_rect(0, 0, DISPLAY_WIDTH as u32, 8, LIME)?;
        self.header_bar("POOL")?;
        self.round_panel(20, 56, 280, 140, PANEL_HI)?;
        self.draw_text("CONNECTED", Point::new(90, 88), OK)?;
        let host = PoolConfig::ellipsize(stratum_host, 28);
        self.draw_text(host.as_str(), Point::new(36, 120), MUTED)?;
        let mut rate: String<28> = String::new();
        let _ = write!(
            rate,
            "{}.{:02} H/s",
            hashrate_x100 / 100,
            hashrate_x100 % 100
        );
        self.draw_text(rate.as_str(), Point::new(100, 156), BRAND)?;
        self.footer_hint("live hashrate on MINE tab")?;
        Ok(())
    }

    fn draw_config_body(&mut self, cfg: &PoolConfig) -> Result<(), Error> {
        self.round_panel(8, 60, 304, 150, PANEL)?;
        // Top→bottom matches setup: wifi → stratum → worker → password
        let wifi = if cfg.wifi_enabled() {
            PoolConfig::ellipsize(cfg.wifi_ssid.as_str(), 22)
        } else {
            PoolConfig::ellipsize("(off)", 22)
        };
        self.draw_row(78, "wifi", wifi.as_str())?;
        self.draw_row(104, "stratum", &PoolConfig::ellipsize(cfg.stratum.as_str(), 26))?;
        self.draw_row(130, "worker", &PoolConfig::ellipsize(cfg.address.as_str(), 26))?;
        self.draw_row(156, "password", cfg.password_masked().as_str())?;
        self.footer_hint("tap body to change · serial also works")?;
        Ok(())
    }

    fn draw_radio_body(
        &mut self,
        cfg: &PoolConfig,
        radio: &RadioStatus,
        stratum: &StratumStatus,
        full: bool,
    ) -> Result<(), Error> {
        if full {
            self.round_panel(8, 60, 304, 150, PANEL)?;
            self.footer_hint("tap tabs · BOOT short=next")?;
        }

        self.fill_rect(16, 68, 288, 130, PANEL)?;

        let ssid = if cfg.wifi_enabled() {
            PoolConfig::ellipsize(cfg.wifi_ssid.as_str(), 18)
        } else {
            PoolConfig::ellipsize("(disabled)", 18)
        };
        let mut wifi_line: String<48> = String::new();
        let _ = write!(
            wifi_line,
            "WiFi {} {}",
            radio.wifi.label(),
            ssid.as_str()
        );
        self.draw_text(&wifi_line, Point::new(18, 88), VALUE_SM)?;

        let mut ip_line: String<40> = String::new();
        let _ = write!(ip_line, "IP   {}", radio.ip_string().as_str());
        self.draw_text(&ip_line, Point::new(18, 114), VALUE_SM)?;

        let mut pool_line: String<48> = String::new();
        let _ = write!(
            pool_line,
            "HOST {}",
            PoolConfig::ellipsize(cfg.stratum.as_str(), 22).as_str()
        );
        self.draw_text(&pool_line, Point::new(18, 140), VALUE_SM)?;

        let pool_state = if stratum.phase.is_connected() {
            "CONNECTED"
        } else {
            stratum.phase.label()
        };
        let mut st_line: String<56> = String::new();
        let _ = write!(
            st_line,
            "POOL {} d{} a{}/r{}",
            pool_state, stratum.difficulty, stratum.accepted, stratum.rejected
        );
        self.draw_text(
            &st_line,
            Point::new(18, 166),
            if stratum.phase.is_connected() {
                OK
            } else {
                VALUE_SM
            },
        )?;
        if let Some([a, b, c, d]) = radio.ip {
            let mut url: String<48> = String::new();
            let _ = write!(url, "WEB  http://{a}.{b}.{c}.{d}/");
            self.draw_text(url.as_str(), Point::new(18, 190), KEY_TXT_DIM)?;
        } else if !stratum.detail.is_empty() {
            let detail = PoolConfig::ellipsize(stratum.detail.as_str(), 28);
            self.draw_text(detail.as_str(), Point::new(18, 190), VALUE_SM)?;
        }
        Ok(())
    }

    fn draw_menu_body(&mut self, selected: MenuItem) -> Result<(), Error> {
        self.round_panel(8, 60, 304, 150, PANEL)?;
        self.draw_text("Options — tap a row", Point::new(18, 78), LABEL)?;

        for (i, item) in MenuItem::ALL.iter().enumerate() {
            let y = 100 + i as i32 * 36;
            let bg = if *item == selected { SELECT } else { PANEL_HI };
            self.round_panel(18, y - 14, 284, 30, bg)?;
            if *item == selected {
                self.fill_rect(18, y - 14, 4, 30, ACCENT_HOT)?;
            }
            let style = if *item == selected { OK } else { VALUE_SM };
            self.draw_text(item.label(), Point::new(32, y), style)?;
        }
        self.footer_hint("tap row or BOOT short=select long=go")?;
        Ok(())
    }

    pub fn draw_auth_keyboard(
        &mut self,
        attempt: u8,
        max: u8,
        typed: &str,
        kb: &Keyboard,
    ) -> Result<(), Error> {
        self.wake_clear()?;
        self.last_screen = None;
        self.header_bar("AUTH")?;
        self.round_panel(6, 36, 308, 52, PANEL_HI)?;
        self.draw_text("Current password", Point::new(14, 52), VALUE_SM)?;
        let mut tries: String<40> = String::new();
        let _ = write!(tries, "attempt {attempt}/{max}");
        self.draw_text(&tries, Point::new(200, 52), LABEL)?;
        let shown = if typed.is_empty() {
            "tap keys…"
        } else {
            "********"
        };
        self.draw_text(shown, Point::new(14, 74), if typed.is_empty() { MUTED } else { VALUE_SM })?;
        self.draw_keyboard(kb)?;
        Ok(())
    }

    /// Invalidate cached screen so next draw_gui does a full redraw.
    pub fn invalidate(&mut self) {
        self.last_screen = None;
    }
}

#[derive(Debug)]
pub enum Error {
    DisplayInterface(&'static str),
    InitError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::DisplayInterface(msg) => write!(f, "display: {msg}"),
            Error::InitError => write!(f, "display init failed"),
        }
    }
}
