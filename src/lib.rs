//! Scrypt proof-of-work miner core for ESP32-2432S028 (CYD) and host demos.
//!
//! The mining engine is `no_std` + `alloc` so it can run on bare-metal classic
//! ESP32 firmware and also be unit-tested on a desktop host.

#![cfg_attr(not(any(test, feature = "host")), no_std)]

extern crate alloc;

pub mod config;
pub mod gui;
pub mod keyboard;
pub mod miner;
pub mod persist;
pub mod radio;
pub mod stratum;
pub mod web;

#[cfg(feature = "esp")]
pub mod display;

#[cfg(feature = "esp")]
pub mod touch;

pub use config::{
    normalize_cpu_mhz, ConfigError, PoolConfig, SetupField, DEFAULT_STRATUM,
};
pub use miner::{
    hash_meets_target, hash_to_hex, target_from_leading_zero_nibbles, HashResult, MinerStats,
    ScryptMiner, HASH_LEN, HEADER_LEN, SCRYPT_LOG_N, SCRYPT_N, SCRYPT_P, SCRYPT_R, V_BYTES,
    V_SLOTS,
};
pub use radio::{RadioStatus, ScannedNetwork, WifiPhase, WIFI_SCAN_MAX};
pub use stratum::{Endpoint, StratumPhase, StratumStatus};
