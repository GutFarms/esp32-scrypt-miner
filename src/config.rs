//! Post-boot mining identity + onboard radio credentials.
//!
//! Credentials can be serialized to a fixed flash/host blob and restored on boot.
//! Blob **v1** (pool only) is still readable; new saves write **v2** (pool + WiFi).
//! `ble_name` remains in the v2 layout for flash compatibility but is unused at runtime.

use core::fmt;
use heapless::String;

pub const ADDRESS_MAX: usize = 96;
pub const PASSWORD_MAX: usize = 64;
pub const STRATUM_MAX: usize = 96;
pub const WIFI_SSID_MAX: usize = 32;
pub const WIFI_PASSWORD_MAX: usize = 64;
pub const BLE_NAME_MAX: usize = 24;

/// Prefill / Enter-default for the stratum field (plain TCP — no TLS on device).
pub const DEFAULT_STRATUM: &str = "stratum+tcp://ltc.viabtc.io:3333";

/// On-disk / flash blob size (fits in one 4 KiB flash sector with room to spare).
pub const CONFIG_BLOB_SIZE: usize = 512;
const CONFIG_MAGIC: &[u8; 4] = b"SCFG";
const CONFIG_VERSION_V1: u8 = 1;
const CONFIG_VERSION: u8 = 2;

pub type AddressString = String<ADDRESS_MAX>;
pub type PasswordString = String<PASSWORD_MAX>;
pub type StratumString = String<STRATUM_MAX>;
pub type WifiSsidString = String<WIFI_SSID_MAX>;
pub type WifiPasswordString = String<WIFI_PASSWORD_MAX>;
pub type BleNameString = String<BLE_NAME_MAX>;

/// Which credential field is being collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupField {
    Address,
    Password,
    Stratum,
    WifiSsid,
    WifiPassword,
    BleName,
}

impl SetupField {
    /// First-time / change-credentials order: **WiFi**, then **stratum → worker → password**.
    /// BLE is not part of setup (stays off; saves RAM for WiFi/stratum).
    pub const ALL: [SetupField; 5] = [
        SetupField::WifiSsid,
        SetupField::WifiPassword,
        SetupField::Stratum,
        SetupField::Address,
        SetupField::Password,
    ];

    /// Pool identity fields required before mining (stratum → worker → password).
    pub const POOL: [SetupField; 3] = [
        SetupField::Stratum,
        SetupField::Address,
        SetupField::Password,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SetupField::Address => "worker",
            SetupField::Password => "password",
            SetupField::Stratum => "stratum",
            SetupField::WifiSsid => "wifi_ssid",
            SetupField::WifiPassword => "wifi_password",
            SetupField::BleName => "ble_name",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            SetupField::Address => "Worker name (wallet OK)",
            SetupField::Password => "Pool password (often 'x')",
            SetupField::Stratum => "Stratum (Enter=ViaBTC LTC)",
            SetupField::WifiSsid => "WiFi SSID (required)",
            SetupField::WifiPassword => "WiFi password (empty=open)",
            SetupField::BleName => "BLE name (- to skip; saves RAM with WiFi)",
        }
    }

    pub fn is_secret(self) -> bool {
        matches!(self, SetupField::Password | SetupField::WifiPassword)
    }

    /// Empty input is accepted (stored as empty / default).
    pub fn allows_empty(self) -> bool {
        matches!(
            self,
            SetupField::WifiPassword | SetupField::BleName | SetupField::Stratum
        )
    }

    pub fn next(self) -> Option<SetupField> {
        match self {
            SetupField::WifiSsid => Some(SetupField::WifiPassword),
            SetupField::WifiPassword => Some(SetupField::Stratum),
            SetupField::Stratum => Some(SetupField::Address),
            SetupField::Address => Some(SetupField::Password),
            SetupField::Password => None,
            SetupField::BleName => None,
        }
    }

    /// 1-based step index in [`SetupField::ALL`] (BLE is not a setup step).
    pub fn step_number(self) -> u8 {
        match self {
            SetupField::WifiSsid => 1,
            SetupField::WifiPassword => 2,
            SetupField::Stratum => 3,
            SetupField::Address => 4,
            SetupField::Password => 5,
            SetupField::BleName => 0,
        }
    }

    pub const SETUP_STEPS: u8 = 5;
}

/// Pool / worker credentials + onboard radio settings entered after boot.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub address: AddressString,
    pub password: PasswordString,
    pub stratum: StratumString,
    pub wifi_ssid: WifiSsidString,
    pub wifi_password: WifiPasswordString,
    pub ble_name: BleNameString,
    /// Persisted CYD touch axis-map id (see `TouchMap::id`).
    pub touch_map: u8,
    /// CPU clock MHz applied on next boot: 80, 160, or 240 (default).
    pub cpu_mhz: u8,
    /// Prefer hashing over LCD redraws (slower GUI refresh).
    pub hash_focus: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            address: AddressString::new(),
            password: PasswordString::new(),
            stratum: StratumString::new(),
            wifi_ssid: WifiSsidString::new(),
            wifi_password: WifiPasswordString::new(),
            // Empty = BLE off (saves RAM when WiFi/stratum mining).
            ble_name: BleNameString::new(),
            touch_map: 1, // ESPHome CYD default map id
            cpu_mhz: 240,
            // Default ON — LCD redraws rarely so scrypt keeps the core.
            hash_focus: true,
        }
    }
}

/// Clamp / normalize a CPU MHz preference to a supported ESP32 rate.
///
/// Classic ESP32 PLL presets are only **80 / 160 / 240 MHz** (plus XTAL-derived
/// underclocks we do not expose). Intermediate values snap to the nearest preset.
pub fn normalize_cpu_mhz(mhz: u8) -> u8 {
    match mhz {
        0 | 240 => 240,
        1..=100 => 80,
        101..=180 => 160,
        _ => 240,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Empty,
    TooLong,
    InvalidChar,
    UnknownField,
    Corrupt,
    BadPassword,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Empty => write!(f, "value cannot be empty"),
            ConfigError::TooLong => write!(f, "value too long"),
            ConfigError::InvalidChar => write!(f, "invalid character"),
            ConfigError::UnknownField => write!(f, "unknown field"),
            ConfigError::Corrupt => write!(f, "saved config corrupt or missing"),
            ConfigError::BadPassword => write!(f, "incorrect password"),
        }
    }
}

impl PoolConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_complete(&self) -> bool {
        !self.address.is_empty()
            && !self.password.is_empty()
            && !self.stratum.is_empty()
            && self.wifi_enabled()
    }

    /// WiFi is required for this firmware — empty SSID means not configured.
    pub fn wifi_enabled(&self) -> bool {
        !self.wifi_ssid.is_empty()
    }

    /// BLE advertising is opt-in: set a non-empty name (not `-` / `skip`).
    pub fn ble_enabled(&self) -> bool {
        !self.ble_name.is_empty()
    }

    /// Advertised name when BLE is enabled; otherwise a display placeholder.
    pub fn ble_name_or_default(&self) -> &str {
        if self.ble_name.is_empty() {
            "(off)"
        } else {
            self.ble_name.as_str()
        }
    }

    /// Constant-time-ish check of the stored pool/setup password.
    pub fn verify_password(&self, attempt: &str) -> bool {
        let attempt = attempt.trim().as_bytes();
        let stored = self.password.as_bytes();
        let mut diff = if attempt.len() == stored.len() { 0u8 } else { 1u8 };
        let max = core::cmp::max(attempt.len(), stored.len());
        for i in 0..max {
            let a = *attempt.get(i).unwrap_or(&0);
            let b = *stored.get(i).unwrap_or(&0);
            diff |= a ^ b;
        }
        diff == 0
    }

    /// Verify password or return [`ConfigError::BadPassword`].
    pub fn authorize(&self, attempt: &str) -> Result<(), ConfigError> {
        if self.verify_password(attempt) {
            Ok(())
        } else {
            Err(ConfigError::BadPassword)
        }
    }

    /// First-boot / companion setup: allow writes without a pool password until
    /// the board has a complete saved identity. After that, require auth.
    pub fn authorize_or_setup(&self, attempt: &str) -> Result<(), ConfigError> {
        if !self.is_complete() {
            Ok(())
        } else {
            self.authorize(attempt)
        }
    }

    pub fn get(&self, field: SetupField) -> &str {
        match field {
            SetupField::Address => self.address.as_str(),
            SetupField::Password => self.password.as_str(),
            SetupField::Stratum => self.stratum.as_str(),
            SetupField::WifiSsid => self.wifi_ssid.as_str(),
            SetupField::WifiPassword => self.wifi_password.as_str(),
            SetupField::BleName => self.ble_name.as_str(),
        }
    }

    pub fn set(&mut self, field: SetupField, raw: &str) -> Result<(), ConfigError> {
        match field {
            SetupField::Address => {
                let value = normalize_value(raw)?;
                self.address.clear();
                self.address
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
            SetupField::Password => {
                let value = normalize_value(raw)?;
                self.password.clear();
                self.password
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
            SetupField::Stratum => {
                let trimmed = raw.trim();
                let value = if trimmed.is_empty() {
                    DEFAULT_STRATUM
                } else {
                    normalize_value(trimmed)?
                };
                validate_stratum(value)?;
                self.stratum.clear();
                self.stratum
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
            SetupField::WifiSsid => {
                let trimmed = raw.trim();
                if trimmed.is_empty() || is_skip_token(trimmed) {
                    return Err(ConfigError::Empty);
                }
                let value = normalize_value(trimmed)?;
                self.wifi_ssid.clear();
                self.wifi_ssid
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
            SetupField::WifiPassword => {
                let trimmed = raw.trim();
                self.wifi_password.clear();
                if trimmed.is_empty() {
                    return Ok(());
                }
                let value = normalize_value(trimmed)?;
                self.wifi_password
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
            SetupField::BleName => {
                let trimmed = raw.trim();
                self.ble_name.clear();
                if trimmed.is_empty() || is_skip_token(trimmed) {
                    return Ok(());
                }
                let value = normalize_value(trimmed)?;
                self.ble_name
                    .push_str(value)
                    .map_err(|_| ConfigError::TooLong)?;
            }
        }
        Ok(())
    }

    /// Parse `worker …` / `address …` / `wifi_ssid …` / … assignment lines.
    pub fn parse_assignment(line: &str) -> Result<(SetupField, &str), ConfigError> {
        let line = line.trim();
        // Longer labels first so `wifi_password` is not eaten by a shorter prefix.
        // `address` kept as a legacy alias for worker.
        const PREFIXES: [(&str, SetupField); 7] = [
            ("wifi_password", SetupField::WifiPassword),
            ("wifi_ssid", SetupField::WifiSsid),
            ("ble_name", SetupField::BleName),
            ("stratum", SetupField::Stratum),
            ("password", SetupField::Password),
            ("worker", SetupField::Address),
            ("address", SetupField::Address),
        ];
        for (prefix, field) in PREFIXES {
            if let Some(rest) = line
                .strip_prefix(prefix)
                .and_then(|r| r.strip_prefix([':', '=', ' ', '\t']))
                .map(str::trim)
            {
                // Allow empty rest for optional fields (e.g. `wifi_password:`).
                if rest.is_empty() && !field.allows_empty() {
                    continue;
                }
                return Ok((field, rest));
            }
        }
        Err(ConfigError::UnknownField)
    }

    /// Display-safe password (never show the real secret on-screen).
    pub fn password_masked(&self) -> String<16> {
        let mut s = String::new();
        if self.password.is_empty() {
            let _ = s.push_str("(unset)");
        } else {
            let _ = s.push_str("********");
        }
        s
    }

    pub fn wifi_password_masked(&self) -> String<16> {
        let mut s = String::new();
        if !self.wifi_enabled() {
            let _ = s.push_str("(n/a)");
        } else if self.wifi_password.is_empty() {
            let _ = s.push_str("(open)");
        } else {
            let _ = s.push_str("********");
        }
        s
    }

    /// Truncate a value for the small LCD.
    pub fn ellipsize(value: &str, max_chars: usize) -> String<96> {
        let mut out = String::new();
        if max_chars == 0 {
            return out;
        }
        if value.chars().count() <= max_chars {
            let _ = out.push_str(value);
            return out;
        }
        if max_chars <= 3 {
            for c in value.chars().take(max_chars) {
                let _ = out.push(c);
            }
            return out;
        }
        for c in value.chars().take(max_chars - 3) {
            let _ = out.push(c);
        }
        let _ = out.push_str("...");
        out
    }

    /// Pack credentials into a fixed-size blob for flash / host storage (v2).
    pub fn to_blob(&self) -> Result<[u8; CONFIG_BLOB_SIZE], ConfigError> {
        if !self.is_complete() {
            return Err(ConfigError::Empty);
        }
        let mut blob = [0u8; CONFIG_BLOB_SIZE];
        blob[0..4].copy_from_slice(CONFIG_MAGIC);
        blob[4] = CONFIG_VERSION;
        // Reserved header bytes 5..8 — not covered by CRC body.
        blob[5] = self.touch_map;
        blob[6] = normalize_cpu_mhz(self.cpu_mhz);
        blob[7] = u8::from(self.hash_focus);

        let mut off = 12usize;
        off = write_field(&mut blob, off, self.address.as_str(), ADDRESS_MAX)?;
        off = write_field(&mut blob, off, self.password.as_str(), PASSWORD_MAX)?;
        off = write_field(&mut blob, off, self.stratum.as_str(), STRATUM_MAX)?;
        off = write_field_allow_empty(&mut blob, off, self.wifi_ssid.as_str(), WIFI_SSID_MAX)?;
        off = write_field_allow_empty(
            &mut blob,
            off,
            self.wifi_password.as_str(),
            WIFI_PASSWORD_MAX,
        )?;
        let _off =
            write_field_allow_empty(&mut blob, off, self.ble_name.as_str(), BLE_NAME_MAX)?;

        let crc = crc32(&blob[12..]);
        blob[8..12].copy_from_slice(&crc.to_le_bytes());
        Ok(blob)
    }

    /// Restore credentials from a previously saved blob (v1 or v2).
    pub fn from_blob(blob: &[u8]) -> Result<Self, ConfigError> {
        if blob.len() < 12 {
            return Err(ConfigError::Corrupt);
        }
        if &blob[0..4] != CONFIG_MAGIC {
            return Err(ConfigError::Corrupt);
        }
        let version = blob[4];
        match version {
            CONFIG_VERSION_V1 => Self::from_blob_v1(blob),
            CONFIG_VERSION => Self::from_blob_v2(blob),
            _ => Err(ConfigError::Corrupt),
        }
    }

    fn from_blob_v1(blob: &[u8]) -> Result<Self, ConfigError> {
        // Legacy size was 320; accept any buffer that covers the v1 payload.
        const V1_SIZE: usize = 320;
        if blob.len() < V1_SIZE {
            return Err(ConfigError::Corrupt);
        }
        let stored_crc = u32::from_le_bytes(blob[8..12].try_into().unwrap());
        if stored_crc != crc32(&blob[12..V1_SIZE]) {
            return Err(ConfigError::Corrupt);
        }

        let mut cfg = Self::new();
        let mut off = 12usize;
        let (addr, o) = read_field(blob, off, ADDRESS_MAX)?;
        off = o;
        let (pass, o) = read_field(blob, off, PASSWORD_MAX)?;
        off = o;
        let (stratum, _) = read_field(blob, off, STRATUM_MAX)?;
        cfg.set(SetupField::Address, addr)?;
        cfg.set(SetupField::Password, pass)?;
        cfg.set(SetupField::Stratum, stratum)?;
        // wifi empty, ble default already set
        if !cfg.is_complete() {
            return Err(ConfigError::Corrupt);
        }
        Ok(cfg)
    }

    fn from_blob_v2(blob: &[u8]) -> Result<Self, ConfigError> {
        if blob.len() < CONFIG_BLOB_SIZE {
            return Err(ConfigError::Corrupt);
        }
        let stored_crc = u32::from_le_bytes(blob[8..12].try_into().unwrap());
        if stored_crc != crc32(&blob[12..CONFIG_BLOB_SIZE]) {
            return Err(ConfigError::Corrupt);
        }

        let mut cfg = Self::new();
        cfg.touch_map = blob[5];
        cfg.cpu_mhz = normalize_cpu_mhz(blob[6]);
        cfg.hash_focus = blob[7] != 0;
        let mut off = 12usize;
        let (addr, o) = read_field(blob, off, ADDRESS_MAX)?;
        off = o;
        let (pass, o) = read_field(blob, off, PASSWORD_MAX)?;
        off = o;
        let (stratum, o) = read_field(blob, off, STRATUM_MAX)?;
        off = o;
        let (wifi_ssid, o) = read_field_allow_empty(blob, off, WIFI_SSID_MAX)?;
        off = o;
        let (wifi_pass, o) = read_field_allow_empty(blob, off, WIFI_PASSWORD_MAX)?;
        off = o;
        let (ble, _) = read_field_allow_empty(blob, off, BLE_NAME_MAX)?;

        cfg.set(SetupField::Address, addr)?;
        cfg.set(SetupField::Password, pass)?;
        cfg.set(SetupField::Stratum, stratum)?;
        cfg.set(SetupField::WifiSsid, wifi_ssid)?;
        cfg.set(SetupField::WifiPassword, wifi_pass)?;
        cfg.set(SetupField::BleName, ble)?;
        if !cfg.is_complete() {
            return Err(ConfigError::Corrupt);
        }
        Ok(cfg)
    }
}

fn is_skip_token(value: &str) -> bool {
    value.eq_ignore_ascii_case("-")
        || value.eq_ignore_ascii_case("skip")
        || value.eq_ignore_ascii_case("none")
}

fn write_field(
    blob: &mut [u8],
    offset: usize,
    value: &str,
    max_len: usize,
) -> Result<usize, ConfigError> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(ConfigError::Empty);
    }
    if bytes.len() > max_len {
        return Err(ConfigError::TooLong);
    }
    if offset + 1 + max_len > blob.len() {
        return Err(ConfigError::TooLong);
    }
    blob[offset] = bytes.len() as u8;
    blob[offset + 1..offset + 1 + bytes.len()].copy_from_slice(bytes);
    Ok(offset + 1 + max_len)
}

fn write_field_allow_empty(
    blob: &mut [u8],
    offset: usize,
    value: &str,
    max_len: usize,
) -> Result<usize, ConfigError> {
    let bytes = value.as_bytes();
    if bytes.len() > max_len {
        return Err(ConfigError::TooLong);
    }
    if offset + 1 + max_len > blob.len() {
        return Err(ConfigError::TooLong);
    }
    blob[offset] = bytes.len() as u8;
    blob[offset + 1..offset + 1 + bytes.len()].copy_from_slice(bytes);
    Ok(offset + 1 + max_len)
}

fn read_field(blob: &[u8], offset: usize, max_len: usize) -> Result<(&str, usize), ConfigError> {
    if offset >= blob.len() {
        return Err(ConfigError::Corrupt);
    }
    let len = blob[offset] as usize;
    if len == 0 || len > max_len {
        return Err(ConfigError::Corrupt);
    }
    let start = offset + 1;
    let end = start + len;
    if end > blob.len() {
        return Err(ConfigError::Corrupt);
    }
    let s = core::str::from_utf8(&blob[start..end]).map_err(|_| ConfigError::Corrupt)?;
    Ok((s, offset + 1 + max_len))
}

fn read_field_allow_empty(
    blob: &[u8],
    offset: usize,
    max_len: usize,
) -> Result<(&str, usize), ConfigError> {
    if offset >= blob.len() {
        return Err(ConfigError::Corrupt);
    }
    let len = blob[offset] as usize;
    if len > max_len {
        return Err(ConfigError::Corrupt);
    }
    let start = offset + 1;
    let end = start + len;
    if end > blob.len() {
        return Err(ConfigError::Corrupt);
    }
    let s = core::str::from_utf8(&blob[start..end]).map_err(|_| ConfigError::Corrupt)?;
    Ok((s, offset + 1 + max_len))
}

/// CRC-32/ISO-HDLC (poly 0xEDB88320), enough to detect corrupt flash pages.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn normalize_value(raw: &str) -> Result<&str, ConfigError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ConfigError::Empty);
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_graphic() || c == ' ' || c == '@' || c == ':' || c == '/' || c == '.')
    {
        // allow typical wallet / URL charset (ascii graphic covers most)
        if !value.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
            return Err(ConfigError::InvalidChar);
        }
    }
    Ok(value)
}

fn validate_stratum(value: &str) -> Result<(), ConfigError> {
    // Accept host, host:port, stratum+tcp://host:port — not TLS (no SSL stack).
    if value.is_empty() {
        return Err(ConfigError::Empty);
    }
    if value.chars().any(|c| c.is_whitespace()) {
        return Err(ConfigError::InvalidChar);
    }
    let lower = value.as_bytes();
    let ssl = b"stratum+ssl://";
    if lower.len() >= ssl.len() {
        let head = &lower[..ssl.len()];
        if head.eq_ignore_ascii_case(ssl) {
            return Err(ConfigError::InvalidChar);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_each_field_individually() {
        let mut cfg = PoolConfig::new();
        assert!(!cfg.is_complete());
        cfg.set(SetupField::Address, "LWallet123").unwrap();
        cfg.set(SetupField::Password, "x").unwrap();
        cfg.set(SetupField::Stratum, "stratum.example.com:3333")
            .unwrap();
        assert!(!cfg.is_complete()); // WiFi still required
        cfg.set(SetupField::WifiSsid, "HomeNet").unwrap();
        assert!(cfg.is_complete());
        assert_eq!(cfg.address.as_str(), "LWallet123");
        assert_eq!(cfg.password.as_str(), "x");
        assert_eq!(cfg.stratum.as_str(), "stratum.example.com:3333");
        assert!(cfg.wifi_enabled());
        assert!(!cfg.ble_enabled());
        assert_eq!(cfg.ble_name_or_default(), "(off)");
    }

    #[test]
    fn empty_stratum_uses_viabtc_ltc_default() {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Stratum, "").unwrap();
        assert_eq!(cfg.stratum.as_str(), DEFAULT_STRATUM);
        assert!(cfg.set(SetupField::Stratum, "stratum+ssl://x:3333").is_err());
    }

    #[test]
    fn wifi_required_ble_optional() {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Address, "LWallet").unwrap();
        cfg.set(SetupField::Password, "x").unwrap();
        cfg.set(SetupField::Stratum, "pool:3333").unwrap();
        assert_eq!(cfg.set(SetupField::WifiSsid, "-"), Err(ConfigError::Empty));
        assert_eq!(cfg.set(SetupField::WifiSsid, ""), Err(ConfigError::Empty));
        cfg.set(SetupField::WifiSsid, "MyAP").unwrap();
        cfg.set(SetupField::WifiPassword, "").unwrap();
        cfg.set(SetupField::BleName, "").unwrap();
        assert!(cfg.wifi_enabled());
        assert!(cfg.is_complete());
        assert_eq!(cfg.wifi_ssid.as_str(), "MyAP");
        assert!(cfg.wifi_password.is_empty());
        assert!(!cfg.ble_enabled());
        cfg.set(SetupField::BleName, "Miner1").unwrap();
        assert!(cfg.ble_enabled());
        assert_eq!(cfg.ble_name_or_default(), "Miner1");
        cfg.set(SetupField::BleName, "-").unwrap();
        assert!(!cfg.ble_enabled());
    }

    #[test]
    fn setup_order_wifi_then_stratum_worker_password() {
        assert_eq!(SetupField::ALL[0], SetupField::WifiSsid);
        assert_eq!(SetupField::ALL[1], SetupField::WifiPassword);
        assert_eq!(SetupField::ALL[2], SetupField::Stratum);
        assert_eq!(SetupField::ALL[3], SetupField::Address);
        assert_eq!(SetupField::ALL[4], SetupField::Password);
        assert_eq!(SetupField::POOL, [
            SetupField::Stratum,
            SetupField::Address,
            SetupField::Password,
        ]);
        assert_eq!(SetupField::Stratum.step_number(), 3);
        assert_eq!(SetupField::Address.step_number(), 4);
        assert_eq!(SetupField::Password.step_number(), 5);
        assert_eq!(
            SetupField::WifiPassword.next(),
            Some(SetupField::Stratum)
        );
        assert_eq!(SetupField::Stratum.next(), Some(SetupField::Address));
        assert_eq!(SetupField::Address.next(), Some(SetupField::Password));
        assert_eq!(SetupField::Password.next(), None);
        assert_eq!(SetupField::ALL.len(), 5);
        assert!(!SetupField::ALL.contains(&SetupField::BleName));
    }

    #[test]
    fn rejects_empty_and_parses_assignments() {
        let mut cfg = PoolConfig::new();
        assert_eq!(
            cfg.set(SetupField::Address, "   "),
            Err(ConfigError::Empty)
        );

        let (field, value) = PoolConfig::parse_assignment("address: LtcAddr99").unwrap();
        assert_eq!(field, SetupField::Address);
        cfg.set(field, value).unwrap();
        assert_eq!(cfg.address.as_str(), "LtcAddr99");

        let (field, value) = PoolConfig::parse_assignment("worker: Rig1").unwrap();
        assert_eq!(field, SetupField::Address);
        cfg.set(field, value).unwrap();
        assert_eq!(cfg.address.as_str(), "Rig1");

        let (field, value) = PoolConfig::parse_assignment("password=secret").unwrap();
        cfg.set(field, value).unwrap();
        let (field, value) =
            PoolConfig::parse_assignment("stratum stratum+tcp://pool:4444").unwrap();
        cfg.set(field, value).unwrap();
        let (field, value) = PoolConfig::parse_assignment("wifi_ssid: CafeWiFi").unwrap();
        cfg.set(field, value).unwrap();
        assert!(cfg.is_complete());
        assert_eq!(cfg.wifi_ssid.as_str(), "CafeWiFi");
    }

    #[test]
    fn masks_password_and_ellipsizes() {
        let mut cfg = PoolConfig::new();
        assert_eq!(cfg.password_masked().as_str(), "(unset)");
        cfg.set(SetupField::Password, "hunter2").unwrap();
        assert_eq!(cfg.password_masked().as_str(), "********");
        let short = PoolConfig::ellipsize("ABCDE12345", 8);
        assert_eq!(short.as_str(), "ABCDE...");
    }

    #[test]
    fn password_authorize_gates_changes() {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Address, "LWallet").unwrap();
        cfg.set(SetupField::Password, "secret").unwrap();
        cfg.set(SetupField::Stratum, "pool:3333").unwrap();

        assert!(cfg.verify_password("secret"));
        assert!(!cfg.verify_password("wrong"));
        assert!(!cfg.verify_password("secre"));
        assert!(cfg.authorize("secret").is_ok());
        assert_eq!(cfg.authorize("nope"), Err(ConfigError::BadPassword));
    }

    #[test]
    fn authorize_or_setup_allows_incomplete_board() {
        let mut cfg = PoolConfig::new();
        assert!(!cfg.is_complete());
        assert!(cfg.authorize_or_setup("anything").is_ok());
        assert!(cfg.authorize_or_setup("").is_ok());

        cfg.set(SetupField::Address, "worker").unwrap();
        cfg.set(SetupField::Password, "x").unwrap();
        cfg.set(SetupField::Stratum, "pool:3333").unwrap();
        cfg.set(SetupField::WifiSsid, "Home").unwrap();
        assert!(cfg.is_complete());
        assert!(cfg.authorize_or_setup("x").is_ok());
        assert_eq!(
            cfg.authorize_or_setup("wrong"),
            Err(ConfigError::BadPassword)
        );
    }

    #[test]
    fn blob_roundtrip_v2_and_detects_corruption() {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Address, "LWallet123").unwrap();
        cfg.set(SetupField::Password, "x").unwrap();
        cfg.set(SetupField::Stratum, "pool.example:3333").unwrap();
        cfg.set(SetupField::WifiSsid, "HomeNet").unwrap();
        cfg.set(SetupField::WifiPassword, "secretwifi").unwrap();
        cfg.set(SetupField::BleName, "LTC-S3").unwrap();

        let blob = cfg.to_blob().unwrap();
        assert_eq!(blob[4], CONFIG_VERSION);
        let restored = PoolConfig::from_blob(&blob).unwrap();
        assert_eq!(restored.address.as_str(), "LWallet123");
        assert_eq!(restored.password.as_str(), "x");
        assert_eq!(restored.stratum.as_str(), "pool.example:3333");
        assert_eq!(restored.wifi_ssid.as_str(), "HomeNet");
        assert_eq!(restored.wifi_password.as_str(), "secretwifi");
        assert_eq!(restored.ble_name.as_str(), "LTC-S3");
        assert_eq!(restored.cpu_mhz, 240);
        assert!(restored.hash_focus);
        cfg.cpu_mhz = 160;
        cfg.hash_focus = false;
        let restored2 = PoolConfig::from_blob(&cfg.to_blob().unwrap()).unwrap();
        assert_eq!(restored2.cpu_mhz, 160);
        assert!(!restored2.hash_focus);

        let mut bad = blob;
        bad[20] ^= 0xFF;
        assert!(matches!(
            PoolConfig::from_blob(&bad),
            Err(ConfigError::Corrupt)
        ));
        assert!(matches!(
            PoolConfig::from_blob(&[0u8; 16]),
            Err(ConfigError::Corrupt)
        ));
    }

    #[test]
    fn blob_roundtrip_wifi_open() {
        let mut cfg = PoolConfig::new();
        cfg.set(SetupField::Address, "LWallet").unwrap();
        cfg.set(SetupField::Password, "x").unwrap();
        cfg.set(SetupField::Stratum, "pool:3333").unwrap();
        cfg.set(SetupField::WifiSsid, "OpenCafe").unwrap();
        cfg.set(SetupField::WifiPassword, "").unwrap();
        cfg.set(SetupField::BleName, "Rig").unwrap();
        let restored = PoolConfig::from_blob(&cfg.to_blob().unwrap()).unwrap();
        assert_eq!(restored.wifi_ssid.as_str(), "OpenCafe");
        assert!(restored.wifi_password.is_empty());
        assert_eq!(restored.ble_name.as_str(), "Rig");
        assert!(restored.is_complete());
    }

    #[test]
    fn reads_legacy_v1_blob_incomplete_without_wifi() {
        // Build a v1-shaped blob manually (320 bytes) — no WiFi fields.
        let mut blob = [0u8; 320];
        blob[0..4].copy_from_slice(b"SCFG");
        blob[4] = 1;
        let mut off = 12usize;
        blob[off] = 7;
        blob[off + 1..off + 8].copy_from_slice(b"LWallet");
        off = 12 + 1 + ADDRESS_MAX;
        blob[off] = 1;
        blob[off + 1] = b'x';
        off = 12 + 1 + ADDRESS_MAX + 1 + PASSWORD_MAX;
        blob[off] = 9;
        blob[off + 1..off + 10].copy_from_slice(b"pool:3333");
        let crc = crc32(&blob[12..]);
        blob[8..12].copy_from_slice(&crc.to_le_bytes());

        // WiFi is required now — legacy blobs without SSID are treated as incomplete.
        assert!(matches!(
            PoolConfig::from_blob(&blob),
            Err(ConfigError::Corrupt)
        ));
    }
}
