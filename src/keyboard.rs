//! Compact on-screen keyboard for CYD 320×240 setup screens.

/// Screen point in landscape pixels (0..319, 0..239).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Char(char),
    Backspace,
    Shift,
    Symbols,
    Space,
    Enter,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLayer {
    Lower,
    Upper,
    Sym,
}

impl KeyLayer {
    pub fn toggle_shift(self) -> Self {
        match self {
            KeyLayer::Lower => KeyLayer::Upper,
            KeyLayer::Upper => KeyLayer::Lower,
            KeyLayer::Sym => KeyLayer::Sym,
        }
    }

    pub fn toggle_sym(self) -> Self {
        match self {
            KeyLayer::Sym => KeyLayer::Lower,
            _ => KeyLayer::Sym,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Keyboard {
    pub layer: KeyLayer,
    /// Top of keyboard in screen pixels.
    pub origin_y: i32,
    /// Focused key index for BOOT navigation (no-touch fallback).
    pub focus: usize,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self {
            layer: KeyLayer::Lower,
            origin_y: 96,
            focus: 0,
        }
    }
}

/// One drawn key: label + bounds.
#[derive(Clone, Copy, Debug)]
pub struct KeyHit {
    pub action: KeyAction,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub label: &'static str,
}

const KEY_H: u32 = 26;
const KEY_GAP: i32 = 2;
const ROW_H: i32 = 28;

impl Keyboard {
    pub fn rows(&self) -> [&'static str; 4] {
        match self.layer {
            KeyLayer::Lower => ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"],
            KeyLayer::Upper => ["1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"],
            KeyLayer::Sym => ["1234567890", "-_.:/@+*=", "#$%&!?,'\"", "()[]{}"],
        }
    }

    /// Iterate all hittable keys for the current layer.
    pub fn keys(&self) -> heapless::Vec<KeyHit, 64> {
        let mut out: heapless::Vec<KeyHit, 64> = heapless::Vec::new();
        let y0 = self.origin_y;

        for (ri, row) in self.rows().iter().enumerate() {
            let n = row.len().max(1) as i32;
            let total_gap = KEY_GAP * (n - 1);
            let key_w = ((320 - 8 - total_gap) / n).max(18) as u32;
            let row_width = n * key_w as i32 + total_gap;
            let x0 = (320 - row_width) / 2;
            let y = y0 + ri as i32 * ROW_H;
            for (ci, ch) in row.chars().enumerate() {
                let x = x0 + ci as i32 * (key_w as i32 + KEY_GAP);
                let _ = out.push(KeyHit {
                    action: KeyAction::Char(ch),
                    x,
                    y,
                    w: key_w,
                    h: KEY_H,
                    label: char_to_static(ch),
                });
            }
        }

        let y = y0 + 4 * ROW_H;
        let shift_lbl = if self.layer == KeyLayer::Upper {
            "ABC"
        } else {
            "sh"
        };
        let sym_lbl = if self.layer == KeyLayer::Sym {
            "abc"
        } else {
            "?#"
        };
        let actions: [(KeyAction, &'static str, i32, u32); 5] = [
            (KeyAction::Shift, shift_lbl, 4, 44),
            (KeyAction::Symbols, sym_lbl, 52, 40),
            (KeyAction::Space, "space", 96, 100),
            (KeyAction::Backspace, "<x", 200, 44),
            (KeyAction::Enter, "OK", 248, 68),
        ];
        for (action, label, x, w) in actions {
            let _ = out.push(KeyHit {
                action,
                x,
                y,
                w,
                h: KEY_H + 2,
                label,
            });
        }

        let _ = out.push(KeyHit {
            action: KeyAction::Skip,
            x: 260,
            y: y0 - 18,
            w: 54,
            h: 16,
            label: "skip",
        });

        out
    }

    pub fn hit_test(&self, p: TouchPoint) -> Option<KeyAction> {
        let x = p.x as i32;
        let y = p.y as i32;
        // Generous pad — resistive CYD taps often land slightly off-center.
        const PAD: i32 = 8;
        let mut best: Option<(i32, KeyAction)> = None;
        for key in self.keys() {
            let x0 = key.x - PAD;
            let y0 = key.y - PAD;
            let x1 = key.x + key.w as i32 + PAD;
            let y1 = key.y + key.h as i32 + PAD;
            if x >= x0 && x < x1 && y >= y0 && y < y1 {
                let cx = key.x + key.w as i32 / 2;
                let cy = key.y + key.h as i32 / 2;
                let dist = (x - cx).abs() + (y - cy).abs();
                if best.map(|(d, _)| dist < d).unwrap_or(true) {
                    best = Some((dist, key.action));
                }
            }
        }
        best.map(|(_, a)| a)
    }

    pub fn focus_next(&mut self) {
        let n = self.keys().len().max(1);
        self.focus = (self.focus + 1) % n;
    }

    pub fn focused_action(&self) -> Option<KeyAction> {
        self.keys().get(self.focus).map(|k| k.action)
    }

    /// Keep focus in range after layer changes.
    pub fn clamp_focus(&mut self) {
        let n = self.keys().len().max(1);
        if self.focus >= n {
            self.focus = n - 1;
        }
    }

    /// Apply key. Returns `true` when the field is complete (Enter / Skip).
    pub fn apply<const N: usize>(
        &mut self,
        action: KeyAction,
        buf: &mut heapless::String<N>,
    ) -> bool {
        match action {
            KeyAction::Char(c) => {
                let _ = buf.push(c);
                false
            }
            KeyAction::Backspace => {
                let _ = buf.pop();
                false
            }
            KeyAction::Space => {
                let _ = buf.push(' ');
                false
            }
            KeyAction::Shift => {
                self.layer = self.layer.toggle_shift();
                self.clamp_focus();
                false
            }
            KeyAction::Symbols => {
                self.layer = self.layer.toggle_sym();
                self.clamp_focus();
                false
            }
            KeyAction::Enter => true,
            KeyAction::Skip => {
                buf.clear();
                let _ = buf.push_str("-");
                true
            }
        }
    }
}

fn char_to_static(c: char) -> &'static str {
    match c {
        '0' => "0", '1' => "1", '2' => "2", '3' => "3", '4' => "4",
        '5' => "5", '6' => "6", '7' => "7", '8' => "8", '9' => "9",
        'a' => "a", 'b' => "b", 'c' => "c", 'd' => "d", 'e' => "e",
        'f' => "f", 'g' => "g", 'h' => "h", 'i' => "i", 'j' => "j",
        'k' => "k", 'l' => "l", 'm' => "m", 'n' => "n", 'o' => "o",
        'p' => "p", 'q' => "q", 'r' => "r", 's' => "s", 't' => "t",
        'u' => "u", 'v' => "v", 'w' => "w", 'x' => "x", 'y' => "y",
        'z' => "z",
        'A' => "A", 'B' => "B", 'C' => "C", 'D' => "D", 'E' => "E",
        'F' => "F", 'G' => "G", 'H' => "H", 'I' => "I", 'J' => "J",
        'K' => "K", 'L' => "L", 'M' => "M", 'N' => "N", 'O' => "O",
        'P' => "P", 'Q' => "Q", 'R' => "R", 'S' => "S", 'T' => "T",
        'U' => "U", 'V' => "V", 'W' => "W", 'X' => "X", 'Y' => "Y",
        'Z' => "Z",
        '-' => "-", '_' => "_", '.' => ".", ':' => ":", '/' => "/",
        '@' => "@", '+' => "+", '*' => "*", '=' => "=",
        '#' => "#", '$' => "$", '%' => "%", '&' => "&", '!' => "!",
        '?' => "?", ',' => ",", '\'' => "'", '"' => "\"",
        '(' => "(", ')' => ")", '[' => "[", ']' => "]", '{' => "{", '}' => "}",
        _ => "?",
    }
}

/// Visible rows on the WiFi scan picker (320×240).
pub const WIFI_SCAN_VISIBLE: usize = 5;
/// Top Y of the first SSID row.
pub const WIFI_SCAN_ROW0_Y: i32 = 56;
/// Height of each SSID row.
pub const WIFI_SCAN_ROW_H: i32 = 28;

/// Hit-test results for the WiFi scan picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiScanHit {
    /// Absolute index into the scanned list.
    Select(usize),
    ScrollUp,
    ScrollDown,
    Rescan,
    TypeManual,
}

/// Hit-test the WiFi scan list + footer actions.
pub fn hit_wifi_scan(p: TouchPoint, scroll: usize, count: usize) -> Option<WifiScanHit> {
    let x = p.x as i32;
    let y = p.y as i32;

    // Side scroll chevrons (padded)
    if (4..44).contains(&x) && (196..228).contains(&y) {
        return Some(WifiScanHit::ScrollUp);
    }
    if (44..84).contains(&x) && (196..228).contains(&y) {
        return Some(WifiScanHit::ScrollDown);
    }

    // Footer actions: scan + type (WiFi is required — no skip).
    if (196..228).contains(&y) {
        if (84..196).contains(&x) {
            return Some(WifiScanHit::Rescan);
        }
        if (196..316).contains(&x) {
            return Some(WifiScanHit::TypeManual);
        }
    }

    if count == 0 {
        return None;
    }
    let visible = WIFI_SCAN_VISIBLE.min(count.saturating_sub(scroll));
    for row in 0..visible {
        let top = WIFI_SCAN_ROW0_Y + row as i32 * WIFI_SCAN_ROW_H;
        if (top - 2..top + WIFI_SCAN_ROW_H).contains(&y) && (4..316).contains(&x) {
            return Some(WifiScanHit::Select(scroll + row));
        }
    }
    None
}

/// Hit-test for main GUI chrome (tabs / menu rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuiHit {
    Tab(usize),
    MenuRow(usize),
    ChangeBanner,
}

pub fn hit_gui(p: TouchPoint, on_menu: bool) -> Option<GuiHit> {
    let x = p.x as i32;
    let y = p.y as i32;
    if (24..58).contains(&y) {
        let tab = (x.clamp(0, 319) / 80) as usize;
        return Some(GuiHit::Tab(tab.min(3)));
    }
    if on_menu {
        if (82..120).contains(&y) {
            return Some(GuiHit::MenuRow(0));
        }
        if (118..156).contains(&y) {
            return Some(GuiHit::MenuRow(1));
        }
    }
    if !on_menu && (56..204).contains(&y) && (4..316).contains(&x) {
        return Some(GuiHit::ChangeBanner);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_and_skip_complete() {
        let mut kb = Keyboard::default();
        let mut s: heapless::String<128> = heapless::String::new();
        assert!(!kb.apply(KeyAction::Char('a'), &mut s));
        assert_eq!(s.as_str(), "a");
        assert!(kb.apply(KeyAction::Enter, &mut s));
        s.clear();
        assert!(kb.apply(KeyAction::Skip, &mut s));
        assert_eq!(s.as_str(), "-");
    }

    #[test]
    fn hit_bottom_enter() {
        let kb = Keyboard::default();
        let p = TouchPoint {
            x: 280,
            y: (kb.origin_y + 4 * 28 + 10) as u16,
        };
        assert_eq!(kb.hit_test(p), Some(KeyAction::Enter));
    }

    #[test]
    fn boot_focus_cycles_and_activates() {
        let mut kb = Keyboard::default();
        let first = kb.focused_action();
        kb.focus_next();
        assert_ne!(kb.focused_action(), first);
        let mut s: heapless::String<128> = heapless::String::new();
        // Focus Enter (last action keys) by jumping near end.
        kb.focus = kb.keys().len() - 2; // OK / Enter is typically near end
        if let Some(KeyAction::Enter) = kb.focused_action() {
            assert!(kb.apply(KeyAction::Enter, &mut s));
        }
    }

    #[test]
    fn wifi_scan_select_and_actions() {
        let p = TouchPoint { x: 40, y: 70 };
        assert_eq!(hit_wifi_scan(p, 0, 3), Some(WifiScanHit::Select(0)));
        let p2 = TouchPoint { x: 240, y: 210 };
        assert_eq!(hit_wifi_scan(p2, 0, 3), Some(WifiScanHit::TypeManual));
        let p3 = TouchPoint { x: 120, y: 210 };
        assert_eq!(hit_wifi_scan(p3, 0, 1), Some(WifiScanHit::Rescan));
        assert_eq!(
            hit_wifi_scan(TouchPoint { x: 40, y: 98 }, 1, 4),
            Some(WifiScanHit::Select(2))
        );
    }

    #[test]
    fn tab_hit() {
        assert_eq!(
            hit_gui(TouchPoint { x: 10, y: 40 }, false),
            Some(GuiHit::Tab(0))
        );
        assert_eq!(
            hit_gui(TouchPoint { x: 200, y: 40 }, false),
            Some(GuiHit::Tab(2))
        );
    }
}
