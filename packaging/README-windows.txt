CYD Companion for Windows
=========================

Wizard-style app to set up and control the ESP32-2432S028 scrypt miner.

Downloads
---------
- CYD-Companion-Setup.exe — installer (Start Menu + desktop shortcut)
- CYD-Companion-Portable.zip — unzip and run (includes README)
- CYD-Companion-App-Only.zip — just cyd-companion.exe

First-time wizard
-----------------
1. Flash esp32-2432s028-scrypt-miner-merged.bin @ address 0x0 (once)
2. Plug the CYD USB cable (CH340 COM port)
3. Run CYD Companion → pick COM → Connect
4. Setup wizard → WiFi + Pool → Save & reboot
5. Board mines at 240 MHz with hash-focus (max scrypt CPU)

Tips
----
- Auth = pool password (after the board is configured)
- First setup does not need Auth
- Overclock tab: sample H/s at 80/160/240 if you want cooler clocks
- Hold BOOT at power-on only for classic on-device setup
