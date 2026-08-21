# Tauri bundled resources

The NSIS package contains only the fixed device dependencies:

- `platform-tools/`: `adb.exe`, `fastboot.exe`, and their required DLLs.
- `drivers/`: the Vivo USB-driver archive used by the native driver command.
- `root-tools/`: `magiskboot.so` used by verified vendor_boot processing.

`scrcpy`, ROOT manager APK files, and `payload_dumper` are deliberately not bundled. Rust provisions and verifies them on demand using the pinned resource catalog.
