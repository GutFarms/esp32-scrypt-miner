/**
 * Browser drag-and-drop flasher for ESP32-2432S028 (CYD).
 * Espressif esptool-js + Web Serial (Chrome / Edge over http://localhost).
 */
import { ESPLoader, Transport } from "https://unpkg.com/esptool-js@0.5.6/bundle.js";

const BUNDLED = "../esp32-2432s028-scrypt-miner-merged.bin";
const BUNDLED_APP = "../esp32-2432s028-scrypt-miner.bin";

const drop = document.getElementById("drop");
const fileInput = document.getElementById("fileInput");
const fileMeta = document.getElementById("fileMeta");
const addressEl = document.getElementById("address");
const baudEl = document.getElementById("baud");
const btnBundled = document.getElementById("btnBundled");
const btnFlash = document.getElementById("btnFlash");
const btnReset = document.getElementById("btnReset");
const bar = document.getElementById("bar");
const statusEl = document.getElementById("status");
const logEl = document.getElementById("log");

let firmware = null; // Uint8Array
let firmwareName = "";
let lastPort = null;

function log(line) {
  logEl.textContent += `${line}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}

function setStatus(text, kind = "") {
  statusEl.textContent = text;
  statusEl.className = `status${kind ? ` ${kind}` : ""}`;
}

function setProgress(pct) {
  bar.style.width = `${Math.max(0, Math.min(100, pct))}%`;
}

function hasWebSerial() {
  return "serial" in navigator;
}

function guessAddress(name, bytes) {
  const n = (name || "").toLowerCase();
  if (n.includes("merged") || bytes.length >= 900_000) {
    addressEl.value = "0x0";
  } else {
    addressEl.value = "0x10000";
  }
}

function validateFirmware(name, bytes) {
  const n = (name || "").toLowerCase();
  if (bytes.length < 1024) return "File too small to be firmware";
  if (n.includes("merged") || bytes.length >= 900_000) {
    if (bytes.length < 0x10000 + 256) return "Merged image looks truncated";
    if (bytes[0x1000] !== 0xe9) return "Merged image missing bootloader magic at 0x1000";
    if (bytes[0x10000] !== 0xe9) return "Merged image missing app magic at 0x10000";
  } else if (bytes[0] !== 0xe9) {
    return "App image should start with ESP magic 0xE9 — did you pick the wrong file?";
  }
  return null;
}

/** Skip leading 0xFF pages when flashing a merged image from 0x0 (faster on CH340). */
function prepareFlashPayload(bytes, address) {
  let addr = address;
  let data = bytes;
  if (addr === 0) {
    let skip = 0;
    while (skip < data.length && data[skip] === 0xff) skip += 1;
    skip &= ~0xfff; // keep 4 KiB alignment
    if (skip > 0 && skip < data.length) {
      log(`Skipping ${skip} leading 0xFF bytes → flash starts at 0x${skip.toString(16)}`);
      data = data.subarray(skip);
      addr = skip;
    }
  }
  return { data, address: addr };
}

function toBinaryString(u8) {
  // esptool-js 0.5.x flash path historically expects a binary string.
  const chunk = 0x8000;
  let out = "";
  for (let i = 0; i < u8.length; i += chunk) {
    out += String.fromCharCode(...u8.subarray(i, i + chunk));
  }
  return out;
}

function adoptFile(name, buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  const err = validateFirmware(name, bytes);
  if (err) {
    setStatus(err, "err");
    log(`Reject ${name}: ${err}`);
    return false;
  }
  firmware = bytes;
  firmwareName = name;
  guessAddress(name, firmware);
  fileMeta.textContent = `${name} · ${(firmware.length / 1024).toFixed(1)} KiB · flash @ ${addressEl.value}`;
  drop.classList.add("has-file");
  btnFlash.disabled = !hasWebSerial();
  setStatus(
    hasWebSerial() ? "Firmware ready — connect & flash" : "Web Serial unavailable (use Chrome/Edge over HTTP)",
    hasWebSerial() ? "ok" : "err",
  );
  log(`Loaded ${name} (${firmware.length} bytes)`);
  return true;
}

async function readBlob(file) {
  const buf = await file.arrayBuffer();
  adoptFile(file.name, buf);
}

async function fetchBin(url) {
  const res = await fetch(url, { cache: "no-cache" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return new Uint8Array(await res.arrayBuffer());
}

drop.addEventListener("click", () => fileInput.click());
drop.addEventListener("keydown", (e) => {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    fileInput.click();
  }
});
fileInput.addEventListener("change", () => {
  const f = fileInput.files?.[0];
  if (f) readBlob(f);
});

["dragenter", "dragover"].forEach((evt) => {
  drop.addEventListener(evt, (e) => {
    e.preventDefault();
    drop.classList.add("dragover");
  });
});
["dragleave", "drop"].forEach((evt) => {
  drop.addEventListener(evt, (e) => {
    e.preventDefault();
    drop.classList.remove("dragover");
  });
});
drop.addEventListener("drop", (e) => {
  const f = e.dataTransfer?.files?.[0];
  if (f) readBlob(f);
});

btnBundled.addEventListener("click", async () => {
  setStatus("Fetching project merged.bin…");
  try {
    const bytes = await fetchBin(BUNDLED);
    adoptFile("esp32-2432s028-scrypt-miner-merged.bin", bytes);
  } catch (err) {
    setStatus(`Could not load bundled image: ${err.message}. Use “Save merged.bin to PC”, then drag it here.`, "err");
    log(String(err));
  }
});

async function downloadBin(url, filename, alsoLoad) {
  setStatus(`Downloading ${filename}…`);
  try {
    const bytes = await fetchBin(url);
    const blob = new Blob([bytes], { type: "application/octet-stream" });
    const objectUrl = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = objectUrl;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(objectUrl);
    if (alsoLoad) adoptFile(filename, bytes);
    setStatus(`Saved ${filename} (${(bytes.length / 1024).toFixed(1)} KiB) to Downloads`, "ok");
    log(`Downloaded ${filename} (${bytes.length} bytes)`);
  } catch (err) {
    setStatus(`Download failed: ${err.message}`, "err");
    log(String(err));
  }
}

document.getElementById("dlMerged")?.addEventListener("click", (e) => {
  e.preventDefault();
  downloadBin(BUNDLED, "esp32-2432s028-scrypt-miner-merged.bin", true);
});
document.getElementById("dlApp")?.addEventListener("click", (e) => {
  e.preventDefault();
  downloadBin(BUNDLED_APP, "esp32-2432s028-scrypt-miner.bin", true);
});

function terminal() {
  return {
    clean() {},
    writeLine(data) {
      log(data);
    },
    write(data) {
      if (data && String(data).trim()) log(data);
    },
  };
}

/** Minimal MD5 for Uint8Array → hex (esptool verify). */
function md5Fallback(bytes) {
  function cmn(q, a, b, x, s, t) {
    a = (a + q + x + t) | 0;
    return (((a << s) | (a >>> (32 - s))) + b) | 0;
  }
  function ff(a, b, c, d, x, s, t) { return cmn((b & c) | (~b & d), a, b, x, s, t); }
  function gg(a, b, c, d, x, s, t) { return cmn((b & d) | (c & ~d), a, b, x, s, t); }
  function hh(a, b, c, d, x, s, t) { return cmn(b ^ c ^ d, a, b, x, s, t); }
  function ii(a, b, c, d, x, s, t) { return cmn(c ^ (b | ~d), a, b, x, s, t); }

  const len = bytes.length;
  const nblocks = (((len + 8) >>> 6) + 1) * 16;
  const blks = new Array(nblocks).fill(0);
  for (let i = 0; i < len; i++) blks[i >> 2] |= bytes[i] << ((i % 4) * 8);
  blks[len >> 2] |= 0x80 << ((len % 4) * 8);
  blks[nblocks - 2] = len * 8;

  let a0 = 0x67452301;
  let b0 = 0xefcdab89;
  let c0 = 0x98badcfe;
  let d0 = 0x10325476;
  for (let i = 0; i < nblocks; i += 16) {
    let a = a0;
    let b = b0;
    let c = c0;
    let d = d0;
    a = ff(a, b, c, d, blks[i], 7, -680876936); d = ff(d, a, b, c, blks[i + 1], 12, -389564586);
    c = ff(c, d, a, b, blks[i + 2], 17, 606105819); b = ff(b, c, d, a, blks[i + 3], 22, -1044525330);
    a = ff(a, b, c, d, blks[i + 4], 7, -176418897); d = ff(d, a, b, c, blks[i + 5], 12, 1200080426);
    c = ff(c, d, a, b, blks[i + 6], 17, -1473231341); b = ff(b, c, d, a, blks[i + 7], 22, -45705983);
    a = ff(a, b, c, d, blks[i + 8], 7, 1770035416); d = ff(d, a, b, c, blks[i + 9], 12, -1958414417);
    c = ff(c, d, a, b, blks[i + 10], 17, -42063); b = ff(b, c, d, a, blks[i + 11], 22, -1990404162);
    a = ff(a, b, c, d, blks[i + 12], 7, 1804603682); d = ff(d, a, b, c, blks[i + 13], 12, -40341101);
    c = ff(c, d, a, b, blks[i + 14], 17, -1502002290); b = ff(b, c, d, a, blks[i + 15], 22, 1236535329);
    a = gg(a, b, c, d, blks[i + 1], 5, -165796510); d = gg(d, a, b, c, blks[i + 6], 9, -1069501632);
    c = gg(c, d, a, b, blks[i + 11], 14, 643717713); b = gg(b, c, d, a, blks[i], 20, -373897302);
    a = gg(a, b, c, d, blks[i + 5], 5, -701558691); d = gg(d, a, b, c, blks[i + 10], 9, 38016083);
    c = gg(c, d, a, b, blks[i + 15], 14, -660478335); b = gg(b, c, d, a, blks[i + 4], 20, -405537848);
    a = gg(a, b, c, d, blks[i + 9], 5, 568446438); d = gg(d, a, b, c, blks[i + 14], 9, -1019803690);
    c = gg(c, d, a, b, blks[i + 3], 14, -187363961); b = gg(b, c, d, a, blks[i + 8], 20, 1163531501);
    a = gg(a, b, c, d, blks[i + 13], 5, -1444681467); d = gg(d, a, b, c, blks[i + 2], 9, -51403784);
    c = gg(c, d, a, b, blks[i + 7], 14, 1735328473); b = gg(b, c, d, a, blks[i + 12], 20, -1926607734);
    a = hh(a, b, c, d, blks[i + 5], 4, -378558); d = hh(d, a, b, c, blks[i + 8], 11, -2022574463);
    c = hh(c, d, a, b, blks[i + 11], 16, 1839030562); b = hh(b, c, d, a, blks[i + 14], 23, -35309556);
    a = hh(a, b, c, d, blks[i + 1], 4, -1530992060); d = hh(d, a, b, c, blks[i + 4], 11, 1272893353);
    c = hh(c, d, a, b, blks[i + 7], 16, -155497632); b = hh(b, c, d, a, blks[i + 10], 23, -1094730640);
    a = hh(a, b, c, d, blks[i + 13], 4, 681279174); d = hh(d, a, b, c, blks[i], 11, -358537222);
    c = hh(c, d, a, b, blks[i + 3], 16, -722521979); b = hh(b, c, d, a, blks[i + 6], 23, 76029189);
    a = hh(a, b, c, d, blks[i + 9], 4, -640364487); d = hh(d, a, b, c, blks[i + 12], 11, -421815835);
    c = hh(c, d, a, b, blks[i + 15], 16, 530742520); b = hh(b, c, d, a, blks[i + 2], 23, -995338651);
    a = ii(a, b, c, d, blks[i], 6, -198630844); d = ii(d, a, b, c, blks[i + 7], 10, 1126891415);
    c = ii(c, d, a, b, blks[i + 14], 15, -1416354905); b = ii(b, c, d, a, blks[i + 5], 21, -57434055);
    a = ii(a, b, c, d, blks[i + 12], 6, 1700485571); d = ii(d, a, b, c, blks[i + 3], 10, -1894986606);
    c = ii(c, d, a, b, blks[i + 10], 15, -1051523); b = ii(b, c, d, a, blks[i + 1], 21, -2054922799);
    a = ii(a, b, c, d, blks[i + 8], 6, 1873313359); d = ii(d, a, b, c, blks[i + 15], 10, -30611744);
    c = ii(c, d, a, b, blks[i + 6], 15, -1560198380); b = ii(b, c, d, a, blks[i + 13], 21, 1309151649);
    a = ii(a, b, c, d, blks[i + 4], 6, -145523070); d = ii(d, a, b, c, blks[i + 11], 10, -1120210379);
    c = ii(c, d, a, b, blks[i + 2], 15, 718787259); b = ii(b, c, d, a, blks[i + 9], 21, -343485551);
    a0 = (a0 + a) | 0; b0 = (b0 + b) | 0; c0 = (c0 + c) | 0; d0 = (d0 + d) | 0;
  }
  function rhex(n) {
    let s = "";
    for (let j = 0; j < 4; j++) s += ((n >> (j * 8)) & 0xff).toString(16).padStart(2, "0");
    return s;
  }
  return rhex(a0) + rhex(b0) + rhex(c0) + rhex(d0);
}

btnFlash.addEventListener("click", async () => {
  if (!firmware) {
    setStatus("Save/load a .bin first", "err");
    return;
  }
  if (!hasWebSerial()) {
    setStatus("Web Serial requires Chrome or Edge over http://localhost", "err");
    return;
  }

  btnFlash.disabled = true;
  btnReset.disabled = true;
  setProgress(0);
  setStatus("Select the COM port in the browser dialog…");

  let transport = null;
  try {
    const port = await navigator.serial.requestPort();
    lastPort = port;
    transport = new Transport(port, true);
    const esploader = new ESPLoader({
      transport,
      baudrate: parseInt(baudEl.value, 10),
      romBaudrate: 115200,
      terminal: terminal(),
    });

    setStatus("Connecting to bootloader… (hold BOOT if this stalls)");
    const chip = await esploader.main();
    log(`Chip: ${chip}`);
    if (String(chip).toLowerCase().includes("esp32s2") || String(chip).toLowerCase().includes("esp32-s3") || String(chip).toLowerCase().includes("esp32c")) {
      log("Warning: this image targets classic ESP32 (CYD), not " + chip);
    }
    setStatus(`Connected: ${chip} — writing flash…`, "ok");

    const address = parseInt(addressEl.value, 16);
    const prepared = prepareFlashPayload(firmware, address);
    const payload = toBinaryString(prepared.data);

    // ESP32-2432S028 (CYD): 4 MB flash, DIO, 40 MHz SPI flash clock.
    const flashOptions = {
      fileArray: [{ data: payload, address: prepared.address }],
      flashSize: "4MB",
      flashMode: "dio",
      flashFreq: "40m",
      eraseAll: false,
      compress: true,
      reportProgress: (_i, written, total) => {
        const pct = total ? (written / total) * 100 : 0;
        setProgress(pct);
        setStatus(`Flashing… ${pct.toFixed(1)}%`);
      },
      calculateMD5Hash: (image) => {
        const u8 = typeof image === "string"
          ? Uint8Array.from(image, (c) => c.charCodeAt(0))
          : image instanceof Uint8Array
            ? image
            : new Uint8Array(image);
        return md5Fallback(u8);
      },
    };

    await esploader.writeFlash(flashOptions);
    await esploader.after("hard_reset");
    setProgress(100);
    setStatus("Flash complete — board should reboot into the miner.", "ok");
    log("Hard reset issued.");
    btnReset.disabled = false;
  } catch (err) {
    console.error(err);
    setStatus(`Flash failed: ${err.message || err}`, "err");
    log(String(err?.stack || err));
  } finally {
    btnFlash.disabled = !firmware || !hasWebSerial();
    try {
      if (transport) await transport.disconnect();
    } catch (_) {
      /* ignore */
    }
  }
});

btnReset.addEventListener("click", async () => {
  try {
    const port = lastPort || (await navigator.serial.requestPort());
    lastPort = port;
    const transport = new Transport(port, true);
    await transport.connect();
    await transport.setDTR(false);
    await new Promise((r) => setTimeout(r, 100));
    await transport.setDTR(true);
    await transport.disconnect();
    setStatus("Reset pulse sent", "ok");
  } catch (err) {
    setStatus(`Reset failed: ${err.message || err}`, "err");
  }
});

// Boot: try to preload project merged image when served over HTTP.
(async () => {
  if (!hasWebSerial()) {
    setStatus("Open this page in Chrome/Edge via http://localhost (Web Serial required)", "err");
    log("navigator.serial missing");
  } else {
    log("Web Serial OK.");
  }
  try {
    const bytes = await fetchBin(BUNDLED);
    if (adoptFile("esp32-2432s028-scrypt-miner-merged.bin", bytes)) {
      setStatus("Project merged.bin loaded — Save to PC and/or Connect & flash", "ok");
    }
  } catch (err) {
    log(`Bundled image not auto-loaded (${err.message}). Use Save merged.bin to PC after build.`);
    setStatus("Build images, then Save merged.bin to PC (or drag a .bin here)");
  }
})();
