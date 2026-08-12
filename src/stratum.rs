//! Stratum (JSON-RPC over TCP) protocol helpers and optional WiFi client.
//!
//! The encoding/decoding and job/header construction are `no_std` and covered by
//! unit tests. The TCP client task is compiled only with `--features esp`.

use core::fmt;
use heapless::String;
use sha2::{Digest, Sha256};

use crate::miner::{HASH_LEN, HEADER_LEN};

pub const JOB_ID_MAX: usize = 64;
pub const EXTRANONCE_MAX: usize = 16; // bytes
pub const MERKLE_BRANCH_MAX: usize = 12;
pub const COINBASE_MAX: usize = 512;
pub const HOST_MAX: usize = 96;

pub type JobIdString = String<JOB_ID_MAX>;
pub type HexEnString = String<{ EXTRANONCE_MAX * 2 }>;
pub type HostString = String<HOST_MAX>;

/// High-level stratum connection phase for GUI / serial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StratumPhase {
    Disabled,
    WaitingWifi,
    Resolving,
    Connecting,
    Subscribing,
    Authorizing,
    Idle,
    Mining,
    Error,
}

impl StratumPhase {
    pub fn label(self) -> &'static str {
        match self {
            StratumPhase::Disabled => "off",
            StratumPhase::WaitingWifi => "wait",
            StratumPhase::Resolving => "dns",
            StratumPhase::Connecting => "tcp",
            StratumPhase::Subscribing => "sub",
            StratumPhase::Authorizing => "auth",
            StratumPhase::Idle => "idle",
            StratumPhase::Mining => "mine",
            StratumPhase::Error => "err",
        }
    }

    /// Authorized with the pool (idle waiting for jobs, or actively mining).
    pub fn is_connected(self) -> bool {
        matches!(self, StratumPhase::Idle | StratumPhase::Mining)
    }

    /// Short chip text for the MINE status badge.
    pub fn chip(self) -> &'static str {
        match self {
            StratumPhase::Disabled => "OFF",
            StratumPhase::WaitingWifi => "WIFI",
            StratumPhase::Resolving => "DNS",
            StratumPhase::Connecting => "TCP",
            StratumPhase::Subscribing => "SUB",
            StratumPhase::Authorizing => "AUTH",
            StratumPhase::Idle => "ON",
            StratumPhase::Mining => "MINE",
            StratumPhase::Error => "ERR",
        }
    }
}

/// Snapshot of stratum client state.
#[derive(Clone, Debug)]
pub struct StratumStatus {
    pub phase: StratumPhase,
    pub accepted: u32,
    pub rejected: u32,
    /// Shares dropped because the submit queue was full or auth was not ready.
    pub dropped: u32,
    pub reconnects: u32,
    pub job_id: JobIdString,
    pub difficulty: u32,
    pub detail: String<48>,
}

impl Default for StratumStatus {
    fn default() -> Self {
        Self {
            phase: StratumPhase::Disabled,
            accepted: 0,
            rejected: 0,
            dropped: 0,
            reconnects: 0,
            job_id: JobIdString::new(),
            difficulty: 1,
            detail: String::new(),
        }
    }
}

/// Parsed stratum endpoint (`host`, `port`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: HostString,
    pub port: u16,
}

impl Endpoint {
    /// Parse `host:port`, `stratum+tcp://host:port`, or bare `host` (port 3333).
    ///
    /// `stratum+ssl://` is rejected — this firmware has no TLS stack.
    pub fn parse(raw: &str) -> Result<Self, StratumError> {
        let mut s = raw.trim();
        if let Some(rest) = s.strip_prefix("stratum+ssl://")
            .or_else(|| {
                if s.len() >= 14 && s[..14].eq_ignore_ascii_case("stratum+ssl://") {
                    Some(&s[14..])
                } else {
                    None
                }
            })
        {
            let _ = rest;
            return Err(StratumError::SslUnsupported);
        }
        for prefix in ["stratum+tcp://", "stratum://", "tcp://"] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest;
                break;
            }
        }
        // strip path if present
        if let Some((head, _)) = s.split_once('/') {
            s = head;
        }
        let (host, port) = if let Some((h, p)) = s.rsplit_once(':') {
            // IPv6 literals are not supported in this compact parser.
            if h.is_empty() {
                return Err(StratumError::BadEndpoint);
            }
            let port: u16 = p.parse().map_err(|_| StratumError::BadEndpoint)?;
            (h, port)
        } else {
            (s, 3333)
        };
        if host.is_empty() || host.len() > HOST_MAX {
            return Err(StratumError::BadEndpoint);
        }
        let mut hs = HostString::new();
        hs.push_str(host).map_err(|_| StratumError::BadEndpoint)?;
        Ok(Self { host: hs, port })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StratumError {
    BadEndpoint,
    /// Device speaks plain TCP only; use `stratum+tcp://`.
    SslUnsupported,
    BadHex,
    BadJson,
    TooLong,
    MissingField,
    BuildJob,
}

impl fmt::Display for StratumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StratumError::BadEndpoint => write!(f, "bad stratum endpoint"),
            StratumError::SslUnsupported => write!(f, "stratum+ssl not supported (use tcp)"),
            StratumError::BadHex => write!(f, "bad hex"),
            StratumError::BadJson => write!(f, "bad json"),
            StratumError::TooLong => write!(f, "value too long"),
            StratumError::MissingField => write!(f, "missing field"),
            StratumError::BuildJob => write!(f, "job build failed"),
        }
    }
}

/// Metadata needed to submit a share for the active job.
#[derive(Clone, Debug)]
pub struct JobMeta {
    pub job_id: JobIdString,
    pub extranonce2_hex: HexEnString,
    pub ntime_hex: String<8>,
}

/// A ready-to-mine job produced from `mining.notify`.
#[derive(Clone, Debug)]
pub struct MiningJob {
    pub meta: JobMeta,
    pub header: [u8; HEADER_LEN],
    pub target: [u8; HASH_LEN],
    pub difficulty: u32,
    pub clean: bool,
}

/// Share ready for `mining.submit`.
#[derive(Clone, Debug)]
pub struct ShareSubmission {
    pub worker: String<96>,
    pub job_id: JobIdString,
    pub extranonce2_hex: HexEnString,
    pub ntime_hex: String<8>,
    pub nonce_hex: String<8>,
}

/// Session fields learned from `mining.subscribe`.
#[derive(Clone, Debug, Default)]
pub struct SubscribeResult {
    pub extranonce1: heapless::Vec<u8, EXTRANONCE_MAX>,
    pub extranonce2_size: usize,
}

/// Inbound stratum message after line parse.
#[derive(Clone, Debug)]
pub enum Inbound {
    SubscribeOk(SubscribeResult),
    AuthorizeOk(bool),
    SetDifficulty(u32),
    Notify(NotifyParams),
    SubmitResult { id: u32, accepted: bool },
    Other,
}

#[derive(Clone, Debug)]
pub struct NotifyParams {
    pub job_id: JobIdString,
    pub prevhash_hex: String<64>,
    pub coinb1_hex: String<COINBASE_MAX>,
    pub coinb2_hex: String<COINBASE_MAX>,
    pub merkle_hex: heapless::Vec<String<64>, MERKLE_BRANCH_MAX>,
    pub version_hex: String<8>,
    pub nbits_hex: String<8>,
    pub ntime_hex: String<8>,
    pub clean: bool,
}

/// Build JSON-RPC request lines (with trailing `\n`).
pub fn encode_subscribe(id: u32, user_agent: &str) -> String<160> {
    let mut s = String::new();
    let _ = fmt::Write::write_fmt(
        &mut s,
        format_args!(
            "{{\"id\":{id},\"method\":\"mining.subscribe\",\"params\":[\"{user_agent}\"]}}\n"
        ),
    );
    s
}

pub fn encode_authorize(id: u32, worker: &str, password: &str) -> String<256> {
    let mut s = String::new();
    let _ = fmt::Write::write_fmt(
        &mut s,
        format_args!(
            "{{\"id\":{id},\"method\":\"mining.authorize\",\"params\":[\"{worker}\",\"{password}\"]}}\n"
        ),
    );
    s
}

pub fn encode_submit(id: u32, share: &ShareSubmission) -> String<384> {
    let mut s = String::new();
    let _ = fmt::Write::write_fmt(
        &mut s,
        format_args!(
            "{{\"id\":{id},\"method\":\"mining.submit\",\"params\":[\"{}\",\"{}\",\"{}\",\"{}\",\"{}\"]}}\n",
            share.worker,
            share.job_id,
            share.extranonce2_hex,
            share.ntime_hex,
            share.nonce_hex
        ),
    );
    s
}

/// Parse one complete JSON line (no trailing newline required).
///
/// Heavy notify/subscribe paths live in `#[inline(never)]` helpers so this
/// function's Xtensa `entry` frame stays small — a prior monolithic parse
/// reserved ~29 KiB and tripped the ProCpu stack guard under WiFi IRQs.
pub fn parse_line(line: &str, expect_subscribe_id: Option<u32>) -> Result<Inbound, StratumError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(StratumError::BadJson);
    }

    if let Some(method) = json_str_field(line, "method") {
        match method {
            "mining.notify" => return parse_notify_inbound(line),
            "mining.set_difficulty" => {
                let diff = parse_set_difficulty(line)?;
                return Ok(Inbound::SetDifficulty(diff));
            }
            // client.get_version / mining.set_extranonce / etc. — ignore
            _ => return Ok(Inbound::Other),
        }
    }

    // Response with id
    let id = json_u32_field(line, "id");
    let has_error = json_error_present(line);

    if let Some(sid) = expect_subscribe_id {
        if id == Some(sid) {
            if has_error {
                return Err(StratumError::BadJson);
            }
            return parse_subscribe_inbound(line);
        }
    }

    if let Some(ok) = json_bool_result(line) {
        if let Some(i) = id {
            return Ok(Inbound::SubmitResult {
                id: i,
                accepted: ok && !has_error,
            });
        }
        return Ok(Inbound::AuthorizeOk(ok && !has_error));
    }

    // Non-bool result: subscribe-shaped array, or null+error for reject.
    if has_error {
        if let Some(i) = id {
            return Ok(Inbound::SubmitResult {
                id: i,
                accepted: false,
            });
        }
        return Ok(Inbound::AuthorizeOk(false));
    }

    if json_key_present(line, "result") {
        // subscribe-like without matching id expectation
        if line.contains("[[") {
            if let Ok(inbound) = parse_subscribe_inbound(line) {
                return Ok(inbound);
            }
        }
        // Some pools ack authorize with result:null, error:null
        return Ok(Inbound::AuthorizeOk(true));
    }

    Ok(Inbound::Other)
}

#[inline(never)]
fn parse_notify_inbound(line: &str) -> Result<Inbound, StratumError> {
    Ok(Inbound::Notify(parse_notify_params(line)?))
}

#[inline(never)]
fn parse_subscribe_inbound(line: &str) -> Result<Inbound, StratumError> {
    Ok(Inbound::SubscribeOk(parse_subscribe_result(line)?))
}

/// True when `"error"` is present and not JSON `null`.
fn json_error_present(line: &str) -> bool {
    let Some(rest) = after_json_key(line, "error") else {
        return false;
    };
    let rest = rest.trim_start();
    !(rest.starts_with("null")
        || rest.starts_with("Null")
        || rest.starts_with("NULL"))
}

fn json_key_present(line: &str, key: &str) -> bool {
    after_json_key(line, key).is_some()
}

/// Locate `:"…"` value after `"key"` with optional whitespace around `:`.
fn after_json_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut needle = String::<24>::new();
    let _ = fmt::Write::write_fmt(&mut needle, format_args!("\"{key}\""));
    let rest = line.split(needle.as_str()).nth(1)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    Some(rest)
}

/// Parse a JSON boolean at `"result"` (tolerates whitespace).
fn json_bool_result(line: &str) -> Option<bool> {
    let rest = after_json_key(line, "result")?;
    if rest.starts_with("true") || rest.starts_with("True") {
        Some(true)
    } else if rest.starts_with("false") || rest.starts_with("False") {
        Some(false)
    } else {
        None
    }
}

fn parse_set_difficulty(line: &str) -> Result<u32, StratumError> {
    // "params":[1024] or [1024.5]
    let Some(rest) = line.split("\"params\"").nth(1) else {
        return Err(StratumError::MissingField);
    };
    let Some(start) = rest.find('[') else {
        return Err(StratumError::BadJson);
    };
    let rest = &rest[start + 1..];
    let mut num = String::<24>::new();
    for c in rest.chars() {
        if c.is_ascii_digit() || c == '.' {
            let _ = num.push(c);
        } else if !num.is_empty() {
            break;
        }
    }
    if num.is_empty() {
        return Err(StratumError::MissingField);
    }
    // Truncate fractional difficulty to at least 1.
    let whole = num.split('.').next().unwrap_or("1");
    let d: u32 = whole.parse().unwrap_or(1);
    Ok(d.max(1))
}

#[inline(never)]
fn parse_notify_params(line: &str) -> Result<NotifyParams, StratumError> {
    // Borrow top-level params slices from `line` (heap Vec<&str>) — do **not**
    // materialize Vec<String<COINBASE_MAX>, 12> on the CPU stack.
    let Some(params) = line.split("\"params\"").nth(1) else {
        return Err(StratumError::MissingField);
    };
    let Some(start) = params.find('[') else {
        return Err(StratumError::BadJson);
    };
    let end = find_matching_bracket(params, start)?;
    let inner = &params[start + 1..end];
    let fields = split_top_level_csv(inner);
    if fields.len() < 9 {
        return Err(StratumError::MissingField);
    }

    let clean = match fields[8].trim() {
        "true" | "True" => true,
        "false" | "False" => false,
        other => other != "0",
    };

    let mut job_id = JobIdString::new();
    job_id
        .push_str(strip_quotes(fields[0].trim()))
        .map_err(|_| StratumError::TooLong)?;

    let mut prevhash_hex = String::<64>::new();
    prevhash_hex
        .push_str(strip_quotes(fields[1].trim()))
        .map_err(|_| StratumError::TooLong)?;

    let mut coinb1_hex = String::<COINBASE_MAX>::new();
    coinb1_hex
        .push_str(strip_quotes(fields[2].trim()))
        .map_err(|_| StratumError::TooLong)?;
    let mut coinb2_hex = String::<COINBASE_MAX>::new();
    coinb2_hex
        .push_str(strip_quotes(fields[3].trim()))
        .map_err(|_| StratumError::TooLong)?;

    let merkle_hex = parse_merkle_branch_list(fields[4].trim())?;

    let mut version_hex = String::<8>::new();
    version_hex
        .push_str(strip_quotes(fields[5].trim()))
        .map_err(|_| StratumError::TooLong)?;
    let mut nbits_hex = String::<8>::new();
    nbits_hex
        .push_str(strip_quotes(fields[6].trim()))
        .map_err(|_| StratumError::TooLong)?;
    let mut ntime_hex = String::<8>::new();
    ntime_hex
        .push_str(strip_quotes(fields[7].trim()))
        .map_err(|_| StratumError::TooLong)?;

    Ok(NotifyParams {
        job_id,
        prevhash_hex,
        coinb1_hex,
        coinb2_hex,
        merkle_hex,
        version_hex,
        nbits_hex,
        ntime_hex,
        clean,
    })
}

fn parse_merkle_branch_list(
    merkle_field: &str,
) -> Result<heapless::Vec<String<64>, MERKLE_BRANCH_MAX>, StratumError> {
    let field = merkle_field.trim();
    if !field.starts_with('[') {
        return Err(StratumError::BadJson);
    }
    let end = find_matching_bracket(field, 0)?;
    let inner = &field[1..end];
    let mut out = heapless::Vec::new();
    for part in split_top_level_csv(inner) {
        let h = strip_quotes(part.trim());
        if h.is_empty() {
            continue;
        }
        let mut s = String::<64>::new();
        s.push_str(h).map_err(|_| StratumError::TooLong)?;
        out.push(s).map_err(|_| StratumError::TooLong)?;
    }
    Ok(out)
}

fn parse_subscribe_result(line: &str) -> Result<SubscribeResult, StratumError> {
    // Typical: "result":[[...],"en1hex",4]
    // Find the last two values before closing of result array — extranonce1 string and size int.
    let Some(result_part) = line.split("\"result\"").nth(1) else {
        return Err(StratumError::MissingField);
    };
    // Collect quoted strings in result — last quoted string before an integer is extranonce1.
    let mut last_hex: Option<&str> = None;
    let mut en2_size: Option<usize> = None;

    let bytes = result_part.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < bytes.len() {
                let s = &result_part[start..i];
                // Prefer hex-looking strings
                if !s.is_empty() && s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                    last_hex = Some(s);
                }
                i += 1;
            }
        } else if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let n: usize = result_part[start..i].parse().unwrap_or(0);
            if (1..=EXTRANONCE_MAX).contains(&n) {
                en2_size = Some(n);
            }
        } else {
            i += 1;
        }
    }

    let en1_hex = last_hex.ok_or(StratumError::MissingField)?;
    let en2_size = en2_size.unwrap_or(4);
    let en1 = hex_decode_vec(en1_hex)?;
    Ok(SubscribeResult {
        extranonce1: en1,
        extranonce2_size: en2_size,
    })
}

/// Build a mining job from notify + subscribe session + pool difficulty.
#[inline(never)]
pub fn build_job(
    notify: &NotifyParams,
    sub: &SubscribeResult,
    difficulty: u32,
    extranonce2_counter: u64,
) -> Result<MiningJob, StratumError> {
    let en2_size = sub.extranonce2_size.max(1).min(EXTRANONCE_MAX);
    let mut en2 = [0u8; EXTRANONCE_MAX];
    for i in 0..en2_size {
        let shift = (en2_size - 1 - i) * 8;
        en2[i] = ((extranonce2_counter >> shift) & 0xff) as u8;
    }
    let en2 = &en2[..en2_size];
    let mut en2_hex = HexEnString::new();
    hex_encode_into(en2, &mut en2_hex)?;

    let coinb1 = hex_decode_alloc(notify.coinb1_hex.as_str())?;
    let coinb2 = hex_decode_alloc(notify.coinb2_hex.as_str())?;
    let mut coinbase = alloc::vec::Vec::with_capacity(
        coinb1.len() + sub.extranonce1.len() + en2.len() + coinb2.len(),
    );
    coinbase.extend_from_slice(&coinb1);
    coinbase.extend_from_slice(&sub.extranonce1);
    coinbase.extend_from_slice(en2);
    coinbase.extend_from_slice(&coinb2);

    let mut merkle = dsha256(&coinbase);
    for branch_hex in notify.merkle_hex.iter() {
        let branch = hex_decode_32(branch_hex.as_str())?;
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(&merkle);
        buf[32..].copy_from_slice(&branch);
        merkle = dsha256(&buf);
    }

    let version = hex_decode_4(notify.version_hex.as_str())?;
    let mut prev = hex_decode_32(notify.prevhash_hex.as_str())?;
    swab256(&mut prev);
    let nbits = hex_decode_4(notify.nbits_hex.as_str())?;
    let ntime = hex_decode_4(notify.ntime_hex.as_str())?;

    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&version);
    header[4..36].copy_from_slice(&prev);
    header[36..68].copy_from_slice(&merkle);
    header[68..72].copy_from_slice(&ntime);
    header[72..76].copy_from_slice(&nbits);
    // nonce left at 0; miner fills it

    let target = target_from_pool_difficulty(difficulty.max(1));

    let mut ntime_hex = String::<8>::new();
    ntime_hex
        .push_str(notify.ntime_hex.as_str())
        .map_err(|_| StratumError::TooLong)?;

    Ok(MiningJob {
        meta: JobMeta {
            job_id: notify.job_id.clone(),
            extranonce2_hex: en2_hex,
            ntime_hex,
        },
        header,
        target,
        difficulty: difficulty.max(1),
        clean: notify.clean,
    })
}

/// Pool difficulty → 32-byte **little-endian** target for [`crate::miner::hash_meets_target`].
///
/// Uses Bitcoin-style Diff1 `0x0000ffff…` (BE) divided by `difficulty`.
pub fn target_from_pool_difficulty(difficulty: u32) -> [u8; HASH_LEN] {
    let diff = difficulty.max(1);
    let mut num = [0u8; 32];
    num[2] = 0xff;
    num[3] = 0xff;
    let mut out_be = [0u8; 32];
    let mut rem = 0u64;
    for i in 0..32 {
        let cur = (rem << 8) | u64::from(num[i]);
        out_be[i] = (cur / u64::from(diff)) as u8;
        rem = cur % u64::from(diff);
    }
    let mut out = out_be;
    out.reverse();
    if out.iter().all(|&b| b == 0) {
        out[0] = 1;
    }
    out
}

pub fn nonce_to_hex(nonce: u32) -> String<8> {
    let mut s = String::new();
    let _ = fmt::Write::write_fmt(&mut s, format_args!("{nonce:08x}"));
    s
}

fn dsha256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

fn swab256(hash: &mut [u8; 32]) {
    for chunk in hash.chunks_exact_mut(4) {
        chunk.reverse();
    }
}

fn hex_nibble(c: u8) -> Result<u8, StratumError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(StratumError::BadHex),
    }
}

fn hex_decode_alloc(hex: &str) -> Result<alloc::vec::Vec<u8>, StratumError> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return Err(StratumError::BadHex);
    }
    let mut out = alloc::vec::Vec::with_capacity(hex.len() / 2);
    let b = hex.as_bytes();
    for i in 0..out.capacity() {
        let v = (hex_nibble(b[i * 2])? << 4) | hex_nibble(b[i * 2 + 1])?;
        out.push(v);
    }
    Ok(out)
}

fn hex_decode_vec(hex: &str) -> Result<heapless::Vec<u8, EXTRANONCE_MAX>, StratumError> {
    let v = hex_decode_alloc(hex)?;
    if v.len() > EXTRANONCE_MAX {
        return Err(StratumError::TooLong);
    }
    let mut out = heapless::Vec::new();
    for b in v {
        out.push(b).map_err(|_| StratumError::TooLong)?;
    }
    Ok(out)
}

fn hex_decode_32(hex: &str) -> Result<[u8; 32], StratumError> {
    let v = hex_decode_alloc(hex)?;
    if v.len() != 32 {
        return Err(StratumError::BadHex);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Ok(out)
}

fn hex_decode_4(hex: &str) -> Result<[u8; 4], StratumError> {
    let v = hex_decode_alloc(hex)?;
    if v.len() != 4 {
        return Err(StratumError::BadHex);
    }
    let mut out = [0u8; 4];
    out.copy_from_slice(&v);
    Ok(out)
}

fn hex_encode_into<const N: usize>(bytes: &[u8], out: &mut String<N>) -> Result<(), StratumError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char)
            .map_err(|_| StratumError::TooLong)?;
        out.push(HEX[(b & 0xf) as usize] as char)
            .map_err(|_| StratumError::TooLong)?;
    }
    Ok(())
}

fn strip_quotes(s: &str) -> &str {
    s.trim().trim_matches('"')
}

fn json_str_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut pat = String::<32>::new();
    let _ = fmt::Write::write_fmt(&mut pat, format_args!("\"{key}\""));
    let rest = line.split(pat.as_str()).nth(1)?;
    let rest = rest.trim_start_matches([' ', ':', '\t']);
    if !rest.starts_with('"') {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn json_u32_field(line: &str, key: &str) -> Option<u32> {
    let mut pat = String::<32>::new();
    let _ = fmt::Write::write_fmt(&mut pat, format_args!("\"{key}\""));
    let rest = line.split(pat.as_str()).nth(1)?;
    let rest = rest.trim_start_matches([' ', ':', '\t']);
    let mut num = String::<12>::new();
    for c in rest.chars() {
        if c.is_ascii_digit() {
            let _ = num.push(c);
        } else if !num.is_empty() {
            break;
        } else if c == 'n' {
            return None; // null
        }
    }
    num.parse().ok()
}

fn find_matching_bracket(s: &str, start: usize) -> Result<usize, StratumError> {
    let b = s.as_bytes();
    if start >= b.len() || b[start] != b'[' {
        return Err(StratumError::BadJson);
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    Err(StratumError::BadJson)
}

fn split_top_level_csv(s: &str) -> alloc::vec::Vec<&str> {
    let mut out = alloc::vec::Vec::new();
    let b = s.as_bytes();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'[' => depth += 1,
                b']' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if start <= s.len() {
        let last = s[start..].trim();
        if !last.is_empty() {
            out.push(s[start..].trim());
        }
    }
    out
}

#[cfg(feature = "esp")]
mod client {
    use core::fmt::Write as _;

    use embassy_executor::Spawner;
    use embassy_futures::select::{select, select3, Either, Either3};
    use embassy_net::tcp::TcpSocket;
    use embassy_net::{IpAddress, IpEndpoint, Stack};
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::Channel;
    use embassy_sync::mutex::Mutex;
    use embassy_sync::signal::Signal;
    use embassy_time::{Duration, Timer};
    use heapless::String;
    use log::info;
    use static_cell::StaticCell;

    use super::{
        build_job, encode_authorize, encode_submit, encode_subscribe, nonce_to_hex, parse_line,
        Endpoint, HostString, Inbound, JobMeta, MiningJob, ShareSubmission, StratumPhase,
        StratumStatus, SubscribeResult,
    };
    use crate::config::PoolConfig;

    #[derive(Clone)]
    struct SessionConfig {
        host: HostString,
        port: u16,
        worker: String<96>,
        password: String<64>,
    }

    static STATUS: Mutex<CriticalSectionRawMutex, StratumStatus> =
        Mutex::new(StratumStatus {
            phase: StratumPhase::Disabled,
            accepted: 0,
            rejected: 0,
            dropped: 0,
            reconnects: 0,
            job_id: String::new(),
            difficulty: 1,
            detail: String::new(),
        });

    static JOB_SIGNAL: Signal<CriticalSectionRawMutex, MiningJob> = Signal::new();
    static SHARE_CH: Channel<CriticalSectionRawMutex, ShareSubmission, 8> = Channel::new();
    static RECONNECT: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    static SESSION_CFG: Mutex<CriticalSectionRawMutex, Option<SessionConfig>> = Mutex::new(None);

    pub async fn snapshot() -> StratumStatus {
        STATUS.lock().await.clone()
    }

    pub fn try_take_job() -> Option<MiningJob> {
        JOB_SIGNAL.try_take()
    }

    /// Queue a share for submit. Waits up to 2s for queue space; counts a drop on timeout.
    pub async fn queue_share(share: ShareSubmission) {
        match select(SHARE_CH.send(share), Timer::after(Duration::from_secs(2))).await {
            Either::First(()) => {}
            Either::Second(()) => {
                if let Ok(mut s) = STATUS.try_lock() {
                    s.dropped = s.dropped.saturating_add(1);
                    s.detail.clear();
                    let _ = s.detail.push_str("share queue full");
                }
                info!("stratum: share dropped (queue full)");
            }
        }
    }

    pub fn make_share(worker: &str, meta: &JobMeta, nonce: u32) -> ShareSubmission {
        let mut w = String::<96>::new();
        let _ = w.push_str(worker);
        ShareSubmission {
            worker: w,
            job_id: meta.job_id.clone(),
            extranonce2_hex: meta.extranonce2_hex.clone(),
            ntime_hex: meta.ntime_hex.clone(),
            nonce_hex: nonce_to_hex(nonce),
        }
    }

    async fn set_phase(phase: StratumPhase, detail: &str) {
        let mut s = STATUS.lock().await;
        s.phase = phase;
        s.detail.clear();
        let _ = s.detail.push_str(detail);
    }

    async fn bump_reconnect() {
        let mut s = STATUS.lock().await;
        s.reconnects = s.reconnects.saturating_add(1);
    }

    fn session_from_pool(cfg: &PoolConfig) -> Option<SessionConfig> {
        let endpoint = Endpoint::parse(cfg.stratum.as_str()).ok()?;
        let mut worker = String::<96>::new();
        let _ = worker.push_str(cfg.address.as_str());
        let mut password = String::<64>::new();
        let _ = password.push_str(cfg.password.as_str());
        Some(SessionConfig {
            host: endpoint.host,
            port: endpoint.port,
            worker,
            password,
        })
    }

    /// Apply updated pool identity (stratum/worker/password) and force reconnect.
    /// WiFi still needs a reboot to restart the radio stack.
    pub async fn apply_pool_config(cfg: &PoolConfig) {
        let Some(session) = session_from_pool(cfg) else {
            set_phase(StratumPhase::Error, "bad endpoint").await;
            return;
        };
        *SESSION_CFG.lock().await = Some(session);
        request_reconnect();
        set_phase(StratumPhase::Connecting, "config reload").await;
        info!("stratum: pool config applied; reconnecting");
    }

    /// Ask the stratum task to drop the TCP session and reconnect.
    pub fn request_reconnect() {
        RECONNECT.signal(());
    }

    pub fn start(spawner: &Spawner, stack: Stack<'static>, cfg: &PoolConfig) {
        let Some(session) = session_from_pool(cfg) else {
            info!("stratum: bad endpoint {}", cfg.stratum);
            if let Ok(mut s) = STATUS.try_lock() {
                s.phase = StratumPhase::Error;
                s.detail.clear();
                let _ = s.detail.push_str("bad endpoint");
            }
            return;
        };

        if let Ok(mut slot) = SESSION_CFG.try_lock() {
            *slot = Some(session);
        } else {
            info!("stratum: config lock busy at start");
        }

        if let Ok(mut s) = STATUS.try_lock() {
            s.phase = StratumPhase::WaitingWifi;
            s.difficulty = 1;
            s.detail.clear();
            let _ = s.detail.push_str("waiting for dhcp");
        }

        match stratum_task(stack) {
            Ok(token) => spawner.spawn(token),
            Err(_) => info!("stratum: failed to spawn task"),
        }
    }

    #[embassy_executor::task]
    async fn stratum_task(stack: Stack<'static>) {
        // Keep socket/line buffers in .bss (not the async future / poll frame).
        static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        static TX_BUF: StaticCell<[u8; 512]> = StaticCell::new();
        static LINE_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        static LINE_TEXT: StaticCell<String<1024>> = StaticCell::new();
        let rx_buf = RX_BUF.init([0; 1024]);
        let tx_buf = TX_BUF.init([0; 512]);
        let line_buf = LINE_BUF.init([0; 1024]);
        let line_text = LINE_TEXT.init(String::new());
        let mut extranonce2_counter = 1u64;
        let mut difficulty = 1u32;
        let mut next_id = 1u32;
        let mut pending_submit_ids: heapless::Vec<u32, 8> = heapless::Vec::new();
        let mut backoff_secs: u64 = 2;
        let mut fail_streak: u32 = 0;

        loop {
            let session = loop {
                if let Some(cfg) = SESSION_CFG.lock().await.clone() {
                    break cfg;
                }
                set_phase(StratumPhase::Error, "no config").await;
                Timer::after(Duration::from_secs(2)).await;
            };

            set_phase(StratumPhase::WaitingWifi, "dhcp").await;
            stack.wait_config_up().await;

            let mut detail = String::<48>::new();
            let _ = write!(detail, "dns {}", session.host.as_str());
            set_phase(StratumPhase::Resolving, detail.as_str()).await;
            let ip = match resolve_host(stack, session.host.as_str()).await {
                Some(ip) => {
                    fail_streak = 0;
                    backoff_secs = 2;
                    ip
                }
                None => {
                    fail_streak = fail_streak.saturating_add(1);
                    let wait = backoff_with_jitter(backoff_secs, fail_streak);
                    let mut d = String::<48>::new();
                    let _ = write!(d, "dns fail; retry {wait}s");
                    set_phase(StratumPhase::Error, d.as_str()).await;
                    info!("stratum: dns failed for {}", session.host.as_str());
                    bump_reconnect().await;
                    Timer::after(Duration::from_secs(wait)).await;
                    backoff_secs = (backoff_secs.saturating_mul(2)).min(60);
                    continue;
                }
            };

            let mut detail = String::<48>::new();
            let _ = write!(detail, "tcp {}:{}", session.host.as_str(), session.port);
            set_phase(StratumPhase::Connecting, detail.as_str()).await;
            let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
            socket.set_timeout(Some(Duration::from_secs(60)));
            let endpoint = IpEndpoint::new(ip, session.port);
            if let Err(e) = socket.connect(endpoint).await {
                fail_streak = fail_streak.saturating_add(1);
                let wait = backoff_with_jitter(backoff_secs, fail_streak);
                info!("stratum: connect error {e:?}; retry in {wait}s");
                let mut d = String::<48>::new();
                let _ = write!(d, "connect fail; retry {wait}s");
                set_phase(StratumPhase::Error, d.as_str()).await;
                bump_reconnect().await;
                Timer::after(Duration::from_secs(wait)).await;
                backoff_secs = (backoff_secs.saturating_mul(2)).min(60);
                continue;
            }

            info!(
                "stratum: connected to {}:{}",
                session.host.as_str(),
                session.port
            );
            fail_streak = 0;
            backoff_secs = 2;
            let mut line_len = 0usize;
            pending_submit_ids.clear();
            let mut sub = SubscribeResult::default();
            let _ = RECONNECT.try_take();

            set_phase(StratumPhase::Subscribing, "mining.subscribe").await;
            let subscribe_id = next_id;
            next_id = next_id.wrapping_add(1);
            let sub_msg = encode_subscribe(subscribe_id, "esp32-cyd-scrypt-miner/0.1");
            if write_all(&mut socket, sub_msg.as_bytes()).await.is_err() {
                set_phase(StratumPhase::Error, "subscribe write fail").await;
                bump_reconnect().await;
                Timer::after(Duration::from_secs(backoff_with_jitter(3, 1))).await;
                continue;
            }

            let mut authorized = false;
            let mut subscribed = false;
            let mut authorize_id: Option<u32> = None;
            let mut auth_rejected = false;

            'session: loop {
                match select3(
                    read_line(&mut socket, line_buf, &mut line_len, line_text),
                    SHARE_CH.receive(),
                    RECONNECT.wait(),
                )
                .await
                {
                    Either3::First(Ok(())) => {
                        let expect = if !subscribed {
                            Some(subscribe_id)
                        } else {
                            None
                        };
                        match parse_line(line_text.as_str(), expect) {
                            Ok(Inbound::SubscribeOk(s)) => {
                                sub = s;
                                subscribed = true;
                                info!(
                                    "stratum: subscribed en1_len={} en2_size={}",
                                    sub.extranonce1.len(),
                                    sub.extranonce2_size
                                );
                                set_phase(StratumPhase::Authorizing, "mining.authorize").await;
                                let auth_id = next_id;
                                authorize_id = Some(auth_id);
                                next_id = next_id.wrapping_add(1);
                                let msg = encode_authorize(
                                    auth_id,
                                    session.worker.as_str(),
                                    session.password.as_str(),
                                );
                                if write_all(&mut socket, msg.as_bytes()).await.is_err() {
                                    break 'session;
                                }
                            }
                            Ok(Inbound::AuthorizeOk(ok)) => {
                                authorize_id = None;
                                if !handle_auth_result(
                                    ok,
                                    session.worker.as_str(),
                                    &mut authorized,
                                    &mut auth_rejected,
                                )
                                .await
                                {
                                    break 'session;
                                }
                            }
                            Ok(Inbound::SetDifficulty(d)) => {
                                difficulty = d.max(1);
                                if let Ok(mut st) = STATUS.try_lock() {
                                    st.difficulty = difficulty;
                                }
                                info!("stratum: difficulty={difficulty}");
                            }
                            Ok(Inbound::Notify(n)) => {
                                if !subscribed {
                                    continue;
                                }
                                match build_job(&n, &sub, difficulty, extranonce2_counter) {
                                    Ok(job) => {
                                        extranonce2_counter =
                                            extranonce2_counter.wrapping_add(1);
                                        if let Ok(mut st) = STATUS.try_lock() {
                                            st.phase = StratumPhase::Mining;
                                            st.job_id.clear();
                                            let _ = st.job_id.push_str(job.meta.job_id.as_str());
                                            st.difficulty = difficulty;
                                            st.detail.clear();
                                            let _ = st.detail.push_str("mining.notify");
                                        }
                                        info!(
                                            "stratum: job={} diff={} clean={}",
                                            job.meta.job_id, difficulty, job.clean
                                        );
                                        JOB_SIGNAL.signal(job);
                                    }
                                    Err(e) => {
                                        info!("stratum: build job error: {e}");
                                        set_phase(StratumPhase::Error, "bad notify").await;
                                    }
                                }
                            }
                            Ok(Inbound::SubmitResult { id, accepted }) => {
                                if authorize_id == Some(id) {
                                    authorize_id = None;
                                    if !handle_auth_result(
                                        accepted,
                                        session.worker.as_str(),
                                        &mut authorized,
                                        &mut auth_rejected,
                                    )
                                    .await
                                    {
                                        break 'session;
                                    }
                                } else if pending_submit_ids.iter().any(|&x| x == id) {
                                    let mut st = STATUS.lock().await;
                                    if accepted {
                                        st.accepted = st.accepted.saturating_add(1);
                                        info!("stratum: share accepted id={id}");
                                    } else {
                                        st.rejected = st.rejected.saturating_add(1);
                                        info!("stratum: share rejected id={id}");
                                    }
                                }
                            }
                            Ok(Inbound::Other) => {}
                            Err(e) => info!("stratum: parse error {e} line={}", line_text.as_str()),
                        }
                    }
                    Either3::First(Err(())) => {
                        info!("stratum: connection closed");
                        break 'session;
                    }
                    Either3::Second(share) => {
                        if !authorized {
                            if let Ok(mut s) = STATUS.try_lock() {
                                s.dropped = s.dropped.saturating_add(1);
                                s.detail.clear();
                                let _ = s.detail.push_str("share before auth");
                            }
                            info!("stratum: drop share (not authorized)");
                            continue;
                        }
                        let id = next_id;
                        next_id = next_id.wrapping_add(1);
                        let _ = pending_submit_ids.push(id);
                        if pending_submit_ids.len() > 6 {
                            let _ = pending_submit_ids.remove(0);
                        }
                        let msg = encode_submit(id, &share);
                        info!(
                            "stratum: submit job={} nonce={}",
                            share.job_id, share.nonce_hex
                        );
                        if write_all(&mut socket, msg.as_bytes()).await.is_err() {
                            break 'session;
                        }
                    }
                    Either3::Third(()) => {
                        info!("stratum: reconnect requested (config change)");
                        break 'session;
                    }
                }
            }

            if auth_rejected {
                // Slow retry — wait for config reload or 2 minutes, then try again.
                set_phase(StratumPhase::Error, "auth rejected — fix creds / wait").await;
                match select(RECONNECT.wait(), Timer::after(Duration::from_secs(120))).await {
                    Either::First(()) => {}
                    Either::Second(()) => {
                        info!("stratum: retrying after auth reject backoff");
                    }
                }
                continue;
            }

            bump_reconnect().await;
            let wait = backoff_with_jitter(backoff_secs, fail_streak.max(1));
            let mut d = String::<48>::new();
            let _ = write!(d, "reconnect in {wait}s");
            set_phase(StratumPhase::Error, d.as_str()).await;
            Timer::after(Duration::from_secs(wait)).await;
            backoff_secs = (backoff_secs.saturating_mul(2)).min(60);
        }
    }

    async fn handle_auth_result(
        ok: bool,
        worker: &str,
        authorized: &mut bool,
        auth_rejected: &mut bool,
    ) -> bool {
        *authorized = ok;
        if ok {
            *auth_rejected = false;
            set_phase(StratumPhase::Idle, "authorized").await;
            info!("stratum: authorized as {worker}");
            true
        } else {
            *auth_rejected = true;
            set_phase(StratumPhase::Error, "auth rejected").await;
            info!("stratum: authorize rejected for {worker}");
            false
        }
    }

    fn backoff_with_jitter(base_secs: u64, streak: u32) -> u64 {
        let base = base_secs.max(1).min(60);
        // Cheap deterministic jitter from fail streak (0..base/4).
        let jitter = u64::from(streak.wrapping_mul(17) % ((base as u32 / 4).max(1) + 1));
        (base + jitter).min(60)
    }

    async fn resolve_host(stack: Stack<'static>, host: &str) -> Option<IpAddress> {
        if let Ok(v) = host.parse::<embassy_net::Ipv4Address>() {
            return Some(IpAddress::Ipv4(v));
        }
        match stack
            .dns_query(host, embassy_net::dns::DnsQueryType::A)
            .await
        {
            Ok(addrs) => addrs.iter().copied().next(),
            Err(e) => {
                info!("stratum: dns error {e:?}");
                None
            }
        }
    }

    async fn write_all(socket: &mut TcpSocket<'_>, mut data: &[u8]) -> Result<(), ()> {
        while !data.is_empty() {
            match embedded_io_async::Write::write(socket, data).await {
                Ok(0) => return Err(()),
                Ok(n) => data = &data[n..],
                Err(_) => return Err(()),
            }
        }
        let _ = embedded_io_async::Write::flush(socket).await;
        Ok(())
    }

    async fn read_line(
        socket: &mut TcpSocket<'_>,
        buf: &mut [u8],
        len: &mut usize,
        out: &mut String<1024>,
    ) -> Result<(), ()> {
        loop {
            if let Some(pos) = buf[..*len].iter().position(|&b| b == b'\n') {
                let line_end = if pos > 0 && buf[pos - 1] == b'\r' {
                    pos - 1
                } else {
                    pos
                };
                let text = core::str::from_utf8(&buf[..line_end]).map_err(|_| ())?;
                out.clear();
                out.push_str(text).map_err(|_| ())?;
                let rest = *len - (pos + 1);
                buf.copy_within(pos + 1..*len, 0);
                *len = rest;
                return Ok(());
            }

            if *len == buf.len() {
                *len = 0; // overflow — resync
            }
            match embedded_io_async::Read::read(socket, &mut buf[*len..]).await {
                Ok(0) => return Err(()),
                Ok(n) => *len += n,
                Err(_) => return Err(()),
            }
        }
    }
}

#[cfg(feature = "esp")]
pub use client::{
    apply_pool_config, make_share, queue_share, request_reconnect, snapshot, start, try_take_job,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        let e = Endpoint::parse("stratum+tcp://pool.example.com:3333").unwrap();
        assert_eq!(e.host.as_str(), "pool.example.com");
        assert_eq!(e.port, 3333);
        let e = Endpoint::parse("192.168.1.5").unwrap();
        assert_eq!(e.port, 3333);
        assert!(Endpoint::parse("").is_err());
        assert!(matches!(
            Endpoint::parse("stratum+ssl://pool:3333"),
            Err(StratumError::SslUnsupported)
        ));
    }

    #[test]
    fn connected_phases_and_chip() {
        assert!(StratumPhase::Idle.is_connected());
        assert!(StratumPhase::Mining.is_connected());
        assert!(!StratumPhase::Connecting.is_connected());
        assert_eq!(StratumPhase::Mining.chip(), "MINE");
        assert_eq!(StratumPhase::Idle.chip(), "ON");
    }

    #[test]
    fn encode_requests_look_sane() {
        let s = encode_subscribe(1, "agent/1");
        assert!(s.as_str().contains("mining.subscribe"));
        assert!(s.as_str().ends_with('\n'));
        let a = encode_authorize(2, "worker", "x");
        assert!(a.as_str().contains("\"worker\""));
        assert!(a.as_str().contains("\"x\""));
    }

    #[test]
    fn parses_set_difficulty_and_subscribe() {
        let line = r#"{"id":null,"method":"mining.set_difficulty","params":[128]}"#;
        match parse_line(line, None).unwrap() {
            Inbound::SetDifficulty(d) => assert_eq!(d, 128),
            other => panic!("unexpected {other:?}"),
        }

        // Whitespace-tolerant difficulty (some pools pretty-print).
        let line = r#"{ "id" : null, "method" : "mining.set_difficulty", "params" : [ 64.5 ] }"#;
        match parse_line(line, None).unwrap() {
            Inbound::SetDifficulty(d) => assert_eq!(d, 64),
            other => panic!("unexpected {other:?}"),
        }

        let line = r#"{"id":1,"result":[[["mining.set_difficulty","1"],["mining.notify","1"]],"deadbeef",4],"error":null}"#;
        match parse_line(line, Some(1)).unwrap() {
            Inbound::SubscribeOk(s) => {
                assert_eq!(s.extranonce1.as_slice(), &[0xde, 0xad, 0xbe, 0xef]);
                assert_eq!(s.extranonce2_size, 4);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Authorize / submit both use result:true with an id — client disambiguates by id.
        match parse_line(r#"{"id":2,"result": true, "error": null}"#, None).unwrap() {
            Inbound::SubmitResult { id, accepted } => {
                assert_eq!(id, 2);
                assert!(accepted);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Explicit JSON-RPC error array → rejected.
        match parse_line(
            r#"{"id":3,"result":null,"error":[20,"Invalid share",null]}"#,
            None,
        )
        .unwrap()
        {
            Inbound::SubmitResult { id, accepted } => {
                assert_eq!(id, 3);
                assert!(!accepted);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_realistic_notify_with_merkle_branch() {
        let line = r#"{"id":null,"method":"mining.notify","params":["job#42","00000000000000000000000000000000000000000000000000000000000000aa","010203","040506",["aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"],"20000000","1d00ffff","5b1a2c3d",false]}"#;
        let Inbound::Notify(n) = parse_line(line, None).unwrap() else {
            panic!("expected notify");
        };
        assert_eq!(n.job_id.as_str(), "job#42");
        assert!(!n.clean);
        assert_eq!(n.merkle_hex.len(), 1);
        assert_eq!(
            n.merkle_hex[0].as_str(),
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
        );

        let sub = SubscribeResult {
            extranonce1: heapless::Vec::from_slice(&[0xab]).unwrap(),
            extranonce2_size: 4,
        };
        let job = build_job(&n, &sub, 8, 1).unwrap();
        assert_eq!(job.difficulty, 8);
        assert_eq!(job.meta.extranonce2_hex.as_str(), "00000001");
    }

    #[test]
    fn builds_job_header_from_notify() {
        // Minimal synthetic notify (empty merkle, short coinbase halves).
        let line = r#"{"id":null,"method":"mining.notify","params":["job42","0000000000000000000000000000000000000000000000000000000000000000","0100","0200",[],"01000000","ffff001d","01020304",true]}"#;
        let Inbound::Notify(n) = parse_line(line, None).unwrap() else {
            panic!("expected notify");
        };
        assert_eq!(n.job_id.as_str(), "job42");
        assert!(n.clean);

        let sub = SubscribeResult {
            extranonce1: heapless::Vec::from_slice(&[0x11, 0x22]).unwrap(),
            extranonce2_size: 2,
        };
        let job = build_job(&n, &sub, 1, 0x3344).unwrap();
        assert_eq!(job.meta.job_id.as_str(), "job42");
        assert_eq!(job.meta.extranonce2_hex.as_str(), "3344");
        assert_eq!(&job.header[0..4], &[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(&job.header[68..72], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(job.difficulty, 1);
        // Target for diff=1 should be non-zero and meet easy hashes sometimes.
        assert!(job.target.iter().any(|&b| b != 0));
    }

    #[test]
    fn difficulty_target_shrinks() {
        let easy = target_from_pool_difficulty(1);
        let hard = target_from_pool_difficulty(16);
        // As LE uint256, harder target is smaller.
        assert!(crate::miner::hash_meets_target(&hard, &easy) || hard != easy);
        let mut bigger = true;
        for i in (0..32).rev() {
            match easy[i].cmp(&hard[i]) {
                core::cmp::Ordering::Greater => break,
                core::cmp::Ordering::Less => {
                    bigger = false;
                    break;
                }
                core::cmp::Ordering::Equal => {}
            }
        }
        assert!(bigger);
    }
}
