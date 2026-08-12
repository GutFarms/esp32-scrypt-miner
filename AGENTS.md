# AGENTS.md

## Cursor Cloud specific instructions

This repo is bare-metal Rust firmware for the ESP32-2432S028 (CYD) scrypt miner, plus
desktop "host" targets that run on this Linux VM. Standard build/run/test commands live in
`README.md` and `.cargo/config.toml`; prefer those. Notes below are the non-obvious bits.

### Targets / what runs where

- `host-miner` (CLI) and `host-gui` (egui desktop app) and the unit tests run on this VM.
- Device firmware (`--features esp`) is cross-compiled for `xtensa-esp32-none-elf` and only
  runs on real ESP32 hardware. It needs the `espup` Xtensa toolchain (`cargo +esp`,
  `-Zbuild-std`) which is **not** installed here, so firmware is not buildable/runnable in
  this environment. Set that up separately only if you specifically need to compile firmware.

### Toolchain gotcha (important)

- The committed `Cargo.lock` pins deps (e.g. `smithay-clipboard 0.7.3`, pulled in by
  `eframe`/`egui`) that require Cargo's `edition2024` feature, i.e. **Rust >= 1.85**. The
  image ships Rust 1.83, which builds `host-miner` + tests but **fails to build `host-gui`**.
  A newer `stable` toolchain is installed and set as the rustup default, so all host targets
  build. If a fresh VM ever reverts to 1.83, run `rustup default stable`.

### Always use `--no-default-features` for host work

- Default features are `["esp", "lite"]`, so a bare `cargo build` / `cargo test` /
  `cargo clippy` tries to cross-compile the firmware and fails (no Xtensa target). For host
  work always pass `--no-default-features` plus the relevant host feature, e.g.:
  - Tests: `cargo test --no-default-features`
  - CLI: `cargo run --no-default-features --features host --bin host-miner --release`
  - GUI: `cargo run --no-default-features --features host-gui --bin host-gui --release`
  - Lint: `cargo clippy --no-default-features --features host-gui --bin host-gui`

### GUI display

- `host-gui` needs an X display. This VM runs an XFCE desktop on `DISPLAY=:1`; launch the GUI
  with `DISPLAY=:1 ./target/release/host-gui` (or `cargo run ...`) to see/interact with it.

### Other notes

- `host-miner` prompts for credentials on stdin but accepts them as flags
  (`--address --password --stratum --wifi-ssid - --ble-name - --no-save`) for non-interactive
  runs; both binaries persist to `scrypt-miner-config.bin` (gitignored) unless `--no-save`.
- `cargo fmt --check` currently reports diffs on already-committed source; that is a
  pre-existing repo state, not something your change introduced.
- `clippy` emits style warnings (e.g. `manual_is_multiple_of`) from the newer toolchain on
  existing code; these are warnings, not errors.
