# Flash images & web flasher

## Save `.bin` to PC + drag/drop flash

```bash
./scripts/build-flash-images.sh
./scripts/serve-web-flasher.sh
```

Open **http://127.0.0.1:8080/web/**

1. **Save merged.bin to PC** → Downloads folder  
2. **Connect & flash** (Chrome / Edge) → pick COM port  

Files: [`web/`](web/) · checksums: [`SHA256SUMS.txt`](SHA256SUMS.txt) (after build)

## CLI

See [`../FLASH.md`](../FLASH.md).
