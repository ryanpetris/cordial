# Development commands

[Protocol index](README.md)

Development firmware, built with the `debug` or `development` profile, reports `build.development`
in `Status.info` and accepts three more commands, plus `EnterBootloader`. Production firmware
compiles their handlers out and answers all four with `ERROR_CODE_UNKNOWN_COMMAND`. Nothing at
runtime, and no serial or DTR sequence, can enable them on production firmware.

| Command | Result | Behavior |
| --- | --- | --- |
| `ListFeatures` | `FeatureList` | One page of the device's integration feature tables, by integration and then index. For HID++, each entry has the feature index, ID, version and flags, and whether the Dongle supports it; hidden and engineering features are listed as unsupported. |
| `ListFiles` | `FileList` | One page of a directory of the application filesystem, not recursive, by name compared bytewise. |
| `ReadFile` | `FileData` | One whole file. |
| `EnterBootloader` | none | See [Commands](commands.md#adapter). |

File paths are absolute ASCII, at most 255 bytes, with no `.` or `..` components or control bytes,
and can name any file of the application filesystem, including identity and bond files. There is no
raw flash access and no write command. A path that does not exist returns `ERROR_CODE_NOT_FOUND`;
any other read failure returns `ERROR_CODE_STORAGE_FAILED`. File access works while Bluetooth is
unavailable.

After `EnterBootloader`, the host expects the Dongle to reappear in USB programming mode: UF2 or
`picotool` for Pico, `esptool` for ESP32-S3. A missing response does not prove the reboot failed;
check the USB mode before retrying. Flashing tools verify the image's profile and hardware before
writing it.
