# Tauri bundled resources

The NSIS package contains every fixed runtime tool resource:

- `platform-tools/`: `adb.exe`, `fastboot.exe`, and their required DLLs.
- `drivers/`: the Vivo USB-driver archive used by the native driver command.
- `root-tools/`: `magiskboot.so` used by verified vendor_boot processing.
- `scrcpy/`: the complete verified Windows scrcpy runtime and `scrcpy-files.sha256` manifest.
- `apk/`: the verified KSU and official KernelSU manager APK files.
- `payload-tools/`: the verified `payload_dumper.exe` executable.

ROM/OTA content and application updates remain network inputs. The fixed resources above are verified from the bundle and are not downloaded on demand.
