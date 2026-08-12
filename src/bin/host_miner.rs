//! Desktop demo of the same scrypt miner core (no ESP hardware required).
//!
//! Credentials are saved to `scrypt-miner-config.bin` and auto-loaded next run.
//! Changing them later requires the **current password**.
//!
//! ```text
//! cargo run --no-default-features --features host --bin host-miner --release
//! cargo run --no-default-features --features host --bin host-miner --release -- --change
//! ```

use std::io::{self, Write as _};
use std::time::Instant;

use esp32_s3_scrypt_miner::config::{PoolConfig, SetupField};
use esp32_s3_scrypt_miner::miner::{hash_to_hex, ScryptMiner, SCRYPT_LOG_N, SCRYPT_N};
use esp32_s3_scrypt_miner::persist::{self, HOST_CONFIG_PATH};

fn main() {
    let mut difficulty = 4u8;
    let mut cfg = PoolConfig::new();
    let mut want_change = false;
    let mut skip_save = false;
    let mut current_password: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" | "-a" => {
                if let Some(v) = args.next() {
                    cfg.set(SetupField::Address, &v).expect("address");
                }
            }
            "--password" | "-p" => {
                if let Some(v) = args.next() {
                    cfg.set(SetupField::Password, &v).expect("password");
                }
            }
            "--stratum" | "-s" => {
                if let Some(v) = args.next() {
                    cfg.set(SetupField::Stratum, &v).expect("stratum");
                }
            }
            "--wifi-ssid" => {
                if let Some(v) = args.next() {
                    cfg.set(SetupField::WifiSsid, &v).expect("wifi_ssid");
                }
            }
            "--wifi-password" => {
                if let Some(v) = args.next() {
                    cfg.set(SetupField::WifiPassword, &v).expect("wifi_password");
                }
            }
            "--difficulty" | "-d" => {
                if let Some(v) = args.next() {
                    difficulty = v.parse().unwrap_or(4);
                }
            }
            "--change" | "--edit" | "--clear" | "--factory" => want_change = true,
            "--current-password" => {
                current_password = args.next();
            }
            "--no-save" => skip_save = true,
            other if other.parse::<u8>().is_ok() => {
                difficulty = other.parse().unwrap();
            }
            other => {
                eprintln!("unknown arg: {other}");
            }
        }
    }

    println!("SCRYPT host miner");
    println!("  N={SCRYPT_N} (2^{SCRYPT_LOG_N})  r=1  p=1");
    println!("  demo difficulty: {difficulty} leading zero nibbles");
    println!("  config file: {HOST_CONFIG_PATH}");
    println!();

    let mut from_file = false;
    if let Ok(saved) = persist::load() {
        if want_change {
            println!("Change saved credentials (current password required).");
            if !authorize_change(&saved, current_password.as_deref()) {
                eprintln!("Change cancelled.");
                std::process::exit(1);
            }
            println!("Authorized. Enter new values:");
            cfg = PoolConfig::new();
            prompt_all_fields(&mut cfg);
            match persist::save(&cfg) {
                Ok(()) => println!("Updated credentials saved to {HOST_CONFIG_PATH}"),
                Err(e) => eprintln!("WARNING: could not save credentials: {e}"),
            }
            from_file = true;
        } else if cfg.is_complete() {
            // CLI provided a full new config — still require current password to overwrite.
            println!("Saved credentials exist; password required to overwrite.");
            if !authorize_change(&saved, current_password.as_deref()) {
                eprintln!("Keeping saved credentials (auth failed).");
                cfg = saved;
                from_file = true;
            } else if !skip_save {
                match persist::save(&cfg) {
                    Ok(()) => println!("Overwrote saved credentials."),
                    Err(e) => eprintln!("WARNING: could not save credentials: {e}"),
                }
            }
        } else {
            println!("Loaded saved credentials from {HOST_CONFIG_PATH}");
            cfg = saved;
            from_file = true;
        }
    } else if want_change {
        eprintln!("No saved credentials to change.");
        std::process::exit(1);
    }

    if !cfg.is_complete() {
        println!("Enter pool + radio credentials (required before mining):");
        prompt_all_fields(&mut cfg);
        if !skip_save {
            match persist::save(&cfg) {
                Ok(()) => println!("Saved credentials to {HOST_CONFIG_PATH}"),
                Err(e) => eprintln!("WARNING: could not save credentials: {e}"),
            }
        }
    }

    println!();
    println!("Using{}:", if from_file { " (from save)" } else { "" });
    println!("  address  = {}", cfg.address);
    println!("  password = {}", cfg.password_masked());
    println!("  stratum  = {}", cfg.stratum);
    println!(
        "  wifi_ssid = {}",
        if cfg.wifi_enabled() {
            cfg.wifi_ssid.as_str()
        } else {
            "(disabled)"
        }
    );
    println!("  wifi_password = {}", cfg.wifi_password_masked());
    println!();

    let mut miner = ScryptMiner::new_demo(difficulty);
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut window_hashes = 0u64;

    loop {
        let result = miner.mine_one();
        window_hashes += 1;

        if result.is_share {
            let mut hex = heapless::String::<128>::new();
            hash_to_hex(&result.hash, 16, &mut hex);
            println!(
                "*** SHARE nonce={:08x} hash={hex} address={} stratum={} total_shares={}",
                result.nonce,
                cfg.address,
                cfg.stratum,
                miner.stats().shares
            );
        }

        if last_report.elapsed().as_millis() >= 1000 {
            let stats = miner.stats();
            let secs = start.elapsed().as_secs_f64().max(0.001);
            let hps = stats.hashes as f64 / secs;
            let mut best = heapless::String::<128>::new();
            hash_to_hex(&stats.best_hash, 8, &mut best);
            println!(
                "rate={hps:.2} H/s  nonce={:08x}  shares={}  best={best}  window={window_hashes}  addr={}  stratum={}",
                stats.nonce, stats.shares, cfg.address, cfg.stratum
            );
            last_report = Instant::now();
            window_hashes = 0;
        }
    }
}

fn authorize_change(saved: &PoolConfig, provided: Option<&str>) -> bool {
    for attempt in 1..=3 {
        let pass = if let Some(p) = provided {
            if attempt > 1 {
                return false;
            }
            p.to_string()
        } else {
            prompt_secret(if attempt == 1 {
                "current password"
            } else {
                "current password (retry)"
            })
        };
        if saved.authorize(&pass).is_ok() {
            println!("  ok");
            return true;
        }
        eprintln!("  incorrect password");
        if provided.is_some() {
            return false;
        }
    }
    false
}

fn prompt_secret(label: &str) -> String {
    print!("{label}: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    line.trim().to_string()
}

fn prompt_all_fields(cfg: &mut PoolConfig) {
    for field in SetupField::ALL {
        if field == SetupField::WifiPassword && !cfg.wifi_enabled() {
            continue;
        }
        prompt_field(cfg, field);
    }
}

fn prompt_field(cfg: &mut PoolConfig, field: SetupField) {
    // Required fields: skip if already set. Optional radio fields: always ask once.
    if !field.allows_empty() && !cfg.get(field).is_empty() {
        return;
    }
    let stdin = io::stdin();
    loop {
        print!("{} ({}): ", field.label(), field.prompt());
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() {
            continue;
        }
        match cfg.set(field, line.trim()) {
            Ok(()) => break,
            Err(e) => eprintln!("  error: {e}"),
        }
    }
}
