//! XPT2046 resistive touch on ESP32-2432S028 (CYD).
//!
//! Dedicated **VSPI / SPI3** bus (separate from TFT HSPI):
//! CLK=25, MOSI=32, MISO=39, CS=33, IRQ=36
//!
//! Tuned toward ESPHome / common CYD profiles (same pins NMMiner must use on
//! this board). Taps fire on **press** (NMMiner/LVGL-style). Axis map is
//! cycleable via BOOT (empty scan) or serial `touch`.

use embedded_hal::delay::DelayNs;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode as SpiMode;
use esp_hal::time::Rate;
use esp_hal::Blocking;

use crate::keyboard::TouchPoint;

/// ESPHome / CYD landscape calibration (display Rotation::Deg90).
const RAW_X_MIN: i32 = 367;
const RAW_X_MAX: i32 = 3355;
const RAW_Y_MIN: i32 = 296;
const RAW_Y_MAX: i32 = 3642;

const SCREEN_W: i32 = 320;
const SCREEN_H: i32 = 240;
/// Soft enough for light presses on CYD resistive glass.
const Z_THRESHOLD: u16 = 120;
/// When PENIRQ is active (pulled up + open-drain), accept a weaker Z.
const Z_IRQ_THRESHOLD: u16 = 30;

const CMD_Z1: u8 = 0xB1;
const CMD_Z2: u8 = 0xC1;
const CMD_X: u8 = 0xD1;
const CMD_Y: u8 = 0x91;
const CMD_PD: u8 = 0xD0;

/// How raw axes map onto landscape screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchMap {
    pub swap_xy: bool,
    pub invert_x: bool,
    pub invert_y: bool,
}

impl TouchMap {
    /// ESPHome yellowtft1 / common CYD landscape (invert X, no swap).
    pub const CYD_DEG90: Self = Self {
        swap_xy: false,
        invert_x: true,
        invert_y: false,
    };

    /// Some GUITION / Sunton 2432S028 panels (swap + invert both).
    pub const CYD_DEG90_SWAP: Self = Self {
        swap_xy: true,
        invert_x: true,
        invert_y: true,
    };

    /// Default matches ESPHome CYD landscape (most 2432S028 ILI9341 boards).
    pub const DEFAULT: Self = Self::CYD_DEG90;

    pub fn next(self) -> Self {
        match (self.swap_xy, self.invert_x, self.invert_y) {
            (false, true, false) => Self::CYD_DEG90_SWAP,
            (true, true, true) => Self {
                swap_xy: false,
                invert_x: false,
                invert_y: true,
            },
            (false, false, true) => Self {
                swap_xy: true,
                invert_x: false,
                invert_y: false,
            },
            _ => Self::CYD_DEG90,
        }
    }

    pub fn id(self) -> u8 {
        match (self.swap_xy, self.invert_x, self.invert_y) {
            (false, true, false) => 1,
            (true, true, true) => 0,
            (false, false, true) => 2,
            (true, false, false) => 3,
            _ => 1,
        }
    }

    pub fn from_id(id: u8) -> Self {
        match id {
            0 => Self::CYD_DEG90_SWAP,
            2 => Self {
                swap_xy: false,
                invert_x: false,
                invert_y: true,
            },
            3 => Self {
                swap_xy: true,
                invert_x: false,
                invert_y: false,
            },
            _ => Self::CYD_DEG90,
        }
    }

    pub fn label(self) -> &'static str {
        match (self.swap_xy, self.invert_x, self.invert_y) {
            (false, true, false) => "map B ix (ESPHome)",
            (true, true, true) => "map A swap",
            (false, false, true) => "map C iy",
            (true, false, false) => "map D sw",
            _ => "map ?",
        }
    }
}

pub struct Touch {
    spi: Spi<'static, Blocking>,
    cs: Output<'static>,
    irq: Input<'static>,
    last: Option<TouchPoint>,
    down: bool,
    pub map: TouchMap,
    /// (x_raw, y_raw, z, irq_low) — used for on-screen cursor while held.
    pub last_raw: Option<(u16, u16, u16, bool)>,
}

pub struct TouchPins {
    pub spi: esp_hal::peripherals::SPI3<'static>,
    pub clk: esp_hal::gpio::AnyPin<'static>,
    pub mosi: esp_hal::gpio::AnyPin<'static>,
    pub miso: esp_hal::gpio::AnyPin<'static>,
    pub cs: esp_hal::gpio::AnyPin<'static>,
    pub irq: esp_hal::gpio::AnyPin<'static>,
}

impl Touch {
    pub fn new(p: TouchPins) -> Self {
        // 2 MHz Mode 0 — common CYD XPT2046 rate (ESPHome often 1–2.5 MHz).
        let spi = Spi::new(
            p.spi,
            SpiConfig::default()
                .with_frequency(Rate::from_mhz(2))
                .with_mode(SpiMode::_0),
        )
        .expect("touch SPI3")
        .with_sck(p.clk)
        .with_mosi(p.mosi)
        .with_miso(p.miso);

        let mut t = Self {
            spi,
            cs: Output::new(p.cs, Level::High, OutputConfig::default()),
            // XPT2046 PENIRQ is open-drain — needs a pull-up or the line floats.
            irq: Input::new(p.irq, InputConfig::default().with_pull(Pull::Up)),
            last: None,
            down: false,
            map: TouchMap::DEFAULT,
            last_raw: None,
        };
        t.cs.set_low();
        let _ = t.read_adc(CMD_PD);
        t.cs.set_high();
        t
    }

    pub fn last_point(&self) -> Option<TouchPoint> {
        self.last
    }

    pub fn cycle_map(&mut self) {
        self.map = self.map.next();
    }

    /// Poll once. Returns a point on **press edge** (finger down) — NMMiner/LVGL style.
    pub fn poll_tap<D: DelayNs>(&mut self, delay: &mut D) -> Option<TouchPoint> {
        let sample = self.read_sample(delay);
        match (self.down, sample) {
            (false, Some(p)) => {
                self.down = true;
                self.last = Some(p);
                Some(p)
            }
            (true, None) => {
                self.down = false;
                None
            }
            (true, Some(p)) => {
                self.last = Some(p);
                None
            }
            (false, None) => None,
        }
    }

    /// Continuous sample while pressed.
    pub fn poll_point<D: DelayNs>(&mut self, delay: &mut D) -> Option<TouchPoint> {
        let sample = self.read_sample(delay);
        self.down = sample.is_some();
        if let Some(p) = sample {
            self.last = Some(p);
        }
        sample
    }

    fn read_sample<D: DelayNs>(&mut self, delay: &mut D) -> Option<TouchPoint> {
        let irq_low = self.irq.is_low();

        self.cs.set_low();
        delay.delay_us(5);

        let z1 = self.read_adc(CMD_Z1);
        let z2 = self.read_adc(CMD_Z2);
        let z = pressure(z1, z2);

        let contact = z >= Z_THRESHOLD || (irq_low && z >= Z_IRQ_THRESHOLD);
        if !contact {
            let _ = self.read_adc(CMD_PD);
            self.cs.set_high();
            delay.delay_us(2);
            self.last_raw = Some((0, 0, z, irq_low));
            return None;
        }

        let _ = self.read_adc(CMD_X);
        let mut ys = [0u16; 3];
        let mut xs = [0u16; 3];
        ys[0] = self.read_adc(CMD_Y);
        xs[0] = self.read_adc(CMD_X);
        ys[1] = self.read_adc(CMD_Y);
        xs[1] = self.read_adc(CMD_X);
        ys[2] = self.read_adc(CMD_Y);
        xs[2] = self.read_adc(CMD_PD);

        self.cs.set_high();
        delay.delay_us(2);

        let x_raw = best_two_avg(xs[0], xs[1], xs[2]);
        let y_raw = best_two_avg(ys[0], ys[1], ys[2]);
        self.last_raw = Some((x_raw, y_raw, z, irq_low));

        if x_raw < 40 && y_raw < 40 {
            return None;
        }
        if x_raw > 4080 && y_raw > 4080 {
            return None;
        }

        Some(map_raw(x_raw, y_raw, self.map))
    }

    fn read_adc(&mut self, cmd: u8) -> u16 {
        let mut data = [cmd, 0, 0];
        if self.spi.transfer(&mut data).is_err() {
            return 0;
        }
        (u16::from(data[1]) << 8 | u16::from(data[2])) >> 3
    }
}

fn pressure(z1: u16, z2: u16) -> u16 {
    z1.saturating_add(4095).saturating_sub(z2)
}

fn best_two_avg(a: u16, b: u16, c: u16) -> u16 {
    let da = a.abs_diff(b);
    let db = a.abs_diff(c);
    let dc = b.abs_diff(c);
    if da <= db && da <= dc {
        ((u32::from(a) + u32::from(b)) / 2) as u16
    } else if db <= da && db <= dc {
        ((u32::from(a) + u32::from(c)) / 2) as u16
    } else {
        ((u32::from(b) + u32::from(c)) / 2) as u16
    }
}

fn map_raw(x_raw: u16, y_raw: u16, map: TouchMap) -> TouchPoint {
    let (mut rx, mut ry) = (x_raw as i32, y_raw as i32);
    if map.swap_xy {
        core::mem::swap(&mut rx, &mut ry);
    }
    let mut x = ((rx - RAW_X_MIN) * (SCREEN_W - 1)) / (RAW_X_MAX - RAW_X_MIN).max(1);
    let mut y = ((ry - RAW_Y_MIN) * (SCREEN_H - 1)) / (RAW_Y_MAX - RAW_Y_MIN).max(1);
    if map.invert_x {
        x = (SCREEN_W - 1) - x;
    }
    if map.invert_y {
        y = (SCREEN_H - 1) - y;
    }
    TouchPoint {
        x: x.clamp(0, SCREEN_W - 1) as u16,
        y: y.clamp(0, SCREEN_H - 1) as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_and_map() {
        assert!(pressure(2000, 500) > Z_THRESHOLD);
        let p = map_raw(2000, 2000, TouchMap::DEFAULT);
        assert!(p.x < 320 && p.y < 240);
    }

    #[test]
    fn map_cycles_from_default() {
        assert!(!TouchMap::DEFAULT.swap_xy);
        assert!(TouchMap::DEFAULT.invert_x);
        assert_eq!(TouchMap::DEFAULT.label(), "map B ix (ESPHome)");
        let m = TouchMap::DEFAULT.next();
        assert!(m.swap_xy);
    }

    #[test]
    fn best_two_avg_picks_closest_pair() {
        assert_eq!(best_two_avg(100, 102, 500), 101);
    }

    #[test]
    fn from_id_preserves_legacy_blob_ids() {
        assert_eq!(TouchMap::from_id(0), TouchMap::CYD_DEG90_SWAP);
        assert_eq!(TouchMap::from_id(1), TouchMap::CYD_DEG90);
        assert_eq!(TouchMap::CYD_DEG90.id(), 1);
    }
}
