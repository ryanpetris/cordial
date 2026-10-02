"""Validate board data and generate platform build configuration."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
PICO_TARGETS = {
    ("rp2040", "rp2040"): ("rp2040", "thumbv6m-none-eabi", 29, 262144),
    ("rp2350", "a"): ("rp235xa", "thumbv8m.main-none-eabihf", 29, 524288),
    ("rp2350", "b"): ("rp235xb", "thumbv8m.main-none-eabihf", 47, 524288),
}


def integer(value, low, high, label, alignment=1):
    if type(value) is not int or not low <= value <= high or value % alignment:
        raise ValueError(f"{label} must be {low}..{high}, aligned to {alignment}")
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"Duplicate configuration field: {key}")
        result[key] = value
    return result


def indicator(value, maximum, label):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"pin", "active_low"} or type(value["active_low"]) is not bool:
        raise ValueError(f"{label} requires pin and boolean active_low, or null")
    return integer(value["pin"], 0, maximum, f"{label}.pin")


def load(path, profile):
    config = json.loads(Path(path).read_text(), object_pairs_hook=unique_object)
    fields = {"default_adapter_name", "name", "chip", "package", "flash_bytes", "xosc_hz", "radio", "mcu_led",
              "bluetooth_backend", "radio_backend", "usb_backend", "storage_backend", "firmware_bytes"}
    if not isinstance(config, dict) or set(config) != fields:
        raise ValueError(f"Board configuration requires exactly: {', '.join(sorted(fields))}")
    if profile not in ("production", "debug", "development"):
        raise ValueError("Select the production, debug or development profile explicitly")
    if not isinstance(config["name"], str) or not re.fullmatch(r"[a-z][a-z0-9_]{0,47}", config["name"]):
        raise ValueError("Invalid hardware configuration name")
    name = config["default_adapter_name"]
    if (not isinstance(name, str) or not name or name != name.strip()
            or len(name.encode("utf-8")) > 64
            or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in name)):
        raise ValueError("default_adapter_name must be a trimmed name of 1..64 UTF-8 bytes without controls")
    if config["chip"] == "esp32s3":
        from esp_config import normalize
        return normalize(config, profile)
    target = PICO_TARGETS.get((config["chip"], config["package"]))
    if target is None:
        raise ValueError("Unsupported Pico chip/package")
    feature, triple, gpio_max, ram = target
    selected = tuple(config[key] for key in ("bluetooth_backend", "usb_backend", "storage_backend"))
    if selected != ("btstack", "embassy", "littlefs"):
        raise ValueError("Pico requires btstack, embassy USB and littlefs")
    if config["radio_backend"] not in ("pico-sdk-cyw43", "embassy-cyw43"):
        raise ValueError("Pico requires pico-sdk-cyw43 or embassy-cyw43 radio")
    flash = integer(config["flash_bytes"], 2097152, 16777216, "flash_bytes", 4096)
    offset = integer(config["firmware_bytes"], 1048576, flash - 131072, "firmware_bytes", 4096)
    # The timer/watchdog use an integer reference-cycle count per microsecond.
    crystal = integer(config["xosc_hz"], 5000000, 15000000, "xosc_hz", 1000000)
    # Keep the supported 125/150 MHz system and 48 MHz USB clocks. The crystal
    # must divide both PLL VCOs; physical wiring identity includes these values.
    if 1500000000 % crystal or 1200000000 % crystal:
        raise ValueError("Crystal must divide the 1.5 GHz system and 1.2 GHz USB PLL VCOs")
    radio = config["radio"]
    if not isinstance(radio, dict) or set(radio) != {"power", "data", "clock", "cs", "led"}:
        raise ValueError("radio requires power, data, clock, cs and led")
    pins = [integer(radio[k], 0, gpio_max, f"radio.{k}") for k in ("power", "data", "clock", "cs")]
    if len(set(pins)) != 4:
        raise ValueError("Radio power, data, clock and cs pins must be distinct")
    if max(radio["data"], radio["clock"]) > 31 and min(radio["data"], radio["clock"]) < 16:
        raise ValueError("Radio data and clock must share PIO window 0..31 or 16..47")
    led = indicator(radio["led"], 2, "radio.led")
    mcu_led = indicator(config["mcu_led"], gpio_max, "mcu_led")
    if mcu_led is not None:
        if mcu_led in pins:
            raise ValueError("MCU LED cannot share a radio pin")
    sys_div1 = 6 if config["chip"] == "rp2040" else 5
    physical = [config["chip"], config["package"], flash, 2, crystal,
                radio["power"], radio["data"], radio["data"], radio["data"],
                radio["clock"], radio["cs"], led if led is not None else -1,
                mcu_led if mcu_led is not None else -1,
                1500000000, sys_div1, 2, 1200000000, 5, 5,
                radio["led"]["active_low"] if led is not None else False,
                config["mcu_led"]["active_low"] if mcu_led is not None else False]
    digest = hashlib.sha256("/".join(map(str, physical)).encode()).hexdigest()
    errata_bytes = 4096 if config["chip"] == "rp2350" else 0
    storage = flash - offset - errata_bytes
    # Identity includes record format and selected backend, independently of pins.
    layout = {"format": "littlefs-json-1",
              "storage_backend": selected[2], "offset": offset, "bytes": storage,
              "erase_bytes": 4096}
    marker = hashlib.sha256(json.dumps(layout, sort_keys=True, separators=(",", ":")).encode()).digest()
    return config | {"profile": profile, "feature": feature, "target": triple,
                     "storage_bytes": storage, "ram_bytes": ram, "hardware_digest": digest, "storage_offset": offset,
                     "storage_identity": marker, "sys_div1": sys_div1}


def generate(config, out):
    from firmware_artifact import generate as generate_metadata
    Path(out).mkdir(parents=True, exist_ok=True)
    generate_metadata(config, out)
    if config["chip"] == "esp32s3":
        from esp_config import generate as generate_esp
        return generate_esp(config, out)
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    radio, flash, ram = config["radio"], config["flash_bytes"], config["ram_bytes"]
    boot2 = 256 if config["chip"] == "rp2040" else 0
    # Reserve stack separately from the Talc heap. The 64 KiB allowance stays
    # unchanged until interrupt-inclusive high-water is measured on hardware.
    stack = 65536
    heap_end = ram + 8192 - stack
    memory = f"""MEMORY {{
    BOOT2 : ORIGIN = 0x10000000, LENGTH = {boot2}
    FLASH : ORIGIN = {0x10000000 + boot2:#x}, LENGTH = {config['storage_offset'] - boot2}
    RAM : ORIGIN = 0x20000000, LENGTH = {heap_end}
    STACK : ORIGIN = {0x20000000 + heap_end:#x}, LENGTH = {stack}
}}
_stack_start = ORIGIN(STACK) + LENGTH(STACK);
_stack_end = ORIGIN(STACK);
_heap_end = ORIGIN(RAM) + LENGTH(RAM);
"""
    # cortex-m-rt keeps .text at _stext, so reserve space for the RP2350 ROM's
    # image block there explicitly. RP2040's separate boot2 is supplied upstream.
    sections = """SECTIONS {
    .start_block : ALIGN(4) { KEEP(*(.start_block)); } > FLASH
    .cordial_metadata : ALIGN(4) { KEEP(*(.cordial_metadata)); } > FLASH
} INSERT AFTER .vector_table;
_stext = ALIGN(ADDR(.cordial_metadata) + SIZEOF(.cordial_metadata), 8);
ASSERT(_heap_end > __sheap, "Static RAM leaves no application heap");
"""
    # link.x includes memory.x before defining its default _stext. Keeping the
    # insertion here also makes dependency links independent of argument order.
    (out / "memory.x").write_text(memory + sections)
    # Concrete peripheral tokens keep pin ownership checked by Rust, including
    # arbitrary supported pin maps. No runtime table or unsafe pin stealing.
    code = f"""pub const FLASH_BYTES: usize = {flash};
pub const STORAGE_START: u32 = {config['storage_offset']};
pub const STORAGE_END: u32 = {config['storage_offset'] + config['storage_bytes']};
pub const STORAGE_IDENTITY: [u8; 32] = {list(config['storage_identity'])!r};
pub const DEFAULT_ADAPTER_NAME: &str = {json.dumps(config["default_adapter_name"], ensure_ascii=False)};
pub const HARDWARE: &str = {json.dumps(config['name'])};
pub fn clocks() -> embassy_rp::clocks::ClockConfig {{
    use embassy_rp::clocks::{{ClockConfig, PllConfig}};
    let mut clocks = ClockConfig::crystal({config['xosc_hz']});
    let xosc = clocks.xosc.as_mut().unwrap();
    xosc.sys_pll = Some(PllConfig {{ refdiv: 1, fbdiv: {1500000000 // config['xosc_hz']}, post_div1: {config['sys_div1']}, post_div2: 2 }});
    xosc.usb_pll = Some(PllConfig {{ refdiv: 1, fbdiv: {1200000000 // config['xosc_hz']}, post_div1: 5, post_div2: 5 }});
    clocks
}}
#[macro_export]
macro_rules! radio_pins {{
    ($p:ident) => {{
        ({', '.join(f'$p.PIN_{radio[k]}' for k in ('power', 'data', 'clock', 'cs'))})
    }};
}}
"""
    led = config["mcu_led"]
    expression = (f"Some(embassy_rp::gpio::Output::new($p.PIN_{led['pin']}, embassy_rp::gpio::Level::{'High' if led['active_low'] else 'Low'}))"
                  if led else "None::<embassy_rp::gpio::Output<'static>>")
    code += f"""pub const MCU_LED_ACTIVE_LOW: bool = {str(bool(led and led['active_low'])).lower()};
pub const RADIO_LED: Option<(u8, bool)> = {('Some((' + str(radio['led']['pin']) + ', ' + str(radio['led']['active_low']).lower() + '))') if radio['led'] else 'None'};
#[macro_export]
macro_rules! indicator_pin {{ ($p:ident) => {{ {expression} }}; }}
"""
    (out / "board.rs").write_text(code)
    if config["radio_backend"] == "pico-sdk-cyw43":
        defines = {
            "PICO_CYW43_SUPPORTED": 1, "PICO_FLASH_SIZE_BYTES": flash,
            "PICO_XOSC_HZ": config["xosc_hz"],
            "CYW43_DEFAULT_PIN_WL_REG_ON": radio["power"],
            "CYW43_DEFAULT_PIN_WL_DATA_OUT": radio["data"],
            "CYW43_DEFAULT_PIN_WL_DATA_IN": radio["data"],
            "CYW43_DEFAULT_PIN_WL_HOST_WAKE": radio["data"],
            "CYW43_DEFAULT_PIN_WL_CLOCK": radio["clock"],
            "CYW43_DEFAULT_PIN_WL_CS": radio["cs"],
            "CYW43_WL_GPIO_COUNT": 3,
        }
        if config["chip"] == "rp2350":
            defines["PICO_RP2350A"] = int(config["package"] == "a")
        (out / "cordial.h").write_text("#pragma once\n" + "".join(
            f"#define {key} {value}\n" for key, value in defines.items()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--profile", choices=("production", "debug", "development"), required=True)
    args = parser.parse_args()
    config = load(args.config, args.profile)
    if os.environ.get("TARGET") != config["target"]:
        raise ValueError(f"Board requires target {config['target']}")
    for feature in ("rp2040", "rp235xa", "rp235xb"):
        if (f"CARGO_FEATURE_{feature.upper()}" in os.environ) != (feature == config.get("feature")):
            raise ValueError("Processor features do not match the selected board")
    if config["chip"] == "esp32s3" and "CARGO_FEATURE_FIRMWARE" in os.environ:
        for backend in ("btstack", "esp-nimble"):
            enabled = f"CARGO_FEATURE_{backend.upper().replace('-', '_')}" in os.environ
            if enabled != (backend == config["bluetooth_backend"]):
                raise ValueError("Rust Bluetooth selection differs from board configuration")
    if config["chip"] != "esp32s3" and "CARGO_FEATURE_FIRMWARE" in os.environ:
        for backend in ("pico-sdk-cyw43", "embassy-cyw43"):
            enabled = f"CARGO_FEATURE_{backend.upper().replace('-', '_')}" in os.environ
            if enabled != (backend == config["radio_backend"]):
                raise ValueError("Rust radio selection differs from board configuration")
    generate(config, args.out)


if __name__ == "__main__":
    main()
