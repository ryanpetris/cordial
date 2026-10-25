import copy
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import firmware_config as config
import firmware_artifact as artifact
import build_firmware as build
import esp_config


class GeneratedConfigTests(unittest.TestCase):
    def snapshot(self, out):
        for path in out.iterdir():
            os.utime(path, ns=(1000000000, 1000000000))
        return {path.name: (path.read_bytes(), path.stat().st_mtime_ns) for path in out.iterdir()}

    def assert_outputs(self, out, before, changed=()):
        self.assertEqual({path.name for path in out.iterdir()}, set(before))
        for name, (data, mtime) in before.items():
            with self.subTest(output=name):
                path = out / name
                if name in changed:
                    self.assertNotEqual(path.read_bytes(), data)
                    self.assertNotEqual(path.stat().st_mtime_ns, mtime)
                else:
                    self.assertEqual(path.read_bytes(), data)
                    self.assertEqual(path.stat().st_mtime_ns, mtime)

    def test_identical_generation_preserves_bytes_and_mtimes(self):
        for board in sorted(config.ROOT.joinpath("boards").glob("*.json")):
            for profile in artifact.PROFILES:
                baseline = config.load(board, profile)
                esp = baseline["chip"] == "esp32s3"
                key = "bluetooth_backend" if esp else "radio_backend"
                backends = ("btstack", "esp-nimble") if esp else ("pico-sdk-cyw43", "embassy-cyw43")
                for backend in backends:
                    with self.subTest(board=board.stem, profile=profile, backend=backend), \
                            tempfile.TemporaryDirectory() as temp:
                        out = Path(temp)
                        selected = baseline | {key: backend}
                        config.generate(selected, out)
                        expected = {"metadata.rs", "board.rs"}
                        expected |= {"partitions.csv", "sdkconfig.defaults"} if esp else {"memory.x"}
                        if backend == "pico-sdk-cyw43":
                            expected.add("cordial.h")
                        self.assertEqual({p.name for p in out.iterdir()}, expected)
                        before = self.snapshot(out)
                        config.generate(selected, out)
                        self.assert_outputs(out, before)

    def test_config_changes_update_affected_outputs(self):
        for name in ("pico2_w", "esp32s3_8mb"):
            raw = json.loads((config.ROOT / "boards" / f"{name}.json").read_text())
            with self.subTest(board=name), tempfile.TemporaryDirectory() as temp:
                path, out = Path(temp, "board.json"), Path(temp, "generated")
                path.write_text(json.dumps(raw))
                config.generate(config.load(path, "development"), out)
                layout_outputs = {"metadata.rs", "board.rs"}
                layout_outputs |= {"partitions.csv", "sdkconfig.defaults"} if raw["chip"] == "esp32s3" else {"memory.x"}
                changes = [({"default_adapter_name": "Desk adapter"}, {"board.rs"}),
                           ({"firmware_bytes": raw["firmware_bytes"] + 65536}, layout_outputs)]
                if raw["chip"] != "esp32s3":
                    changes.append(({"radio": raw["radio"] | {"clock": 28}},
                                    {"board.rs", "cordial.h", "metadata.rs"}))
                for change, outputs in changes:
                    before = self.snapshot(out)
                    raw.update(change)
                    path.write_text(json.dumps(raw))
                    config.generate(config.load(path, "development"), out)
                    self.assert_outputs(out, before, outputs)

    def test_version_changes_update_metadata_only(self):
        for name in ("pico2_w", "esp32s3_8mb"):
            board = config.load(config.ROOT / "boards" / f"{name}.json", "production")
            with self.subTest(board=name), tempfile.TemporaryDirectory() as temp, \
                    patch.dict(os.environ, CORDIAL_VERSION="1.2.3"):
                out = Path(temp)
                config.generate(board, out)
                before = self.snapshot(out)
                os.environ["CORDIAL_VERSION"] = "1.2.4"
                config.generate(board, out)
                self.assert_outputs(out, before, {"metadata.rs"})
                self.assertIn('const FIRMWARE_VERSION: &str = "1.2.4";', (out / "metadata.rs").read_text())

    def test_esp_native_sources_and_headers_invalidate_defaults(self):
        board = config.load(config.ROOT / "boards/esp32s3_8mb.json", "development")
        with tempfile.TemporaryDirectory() as temp, \
                patch.object(esp_config, "__file__", str(Path(temp, "tools/esp_config.py"))):
            components = Path(temp, "platforms/esp32s3/components/native")
            components.mkdir(parents=True)
            source, header = components / "native.c", components / "native.h"
            source.write_text('#include "native.h"\n')
            header.write_text("#define VALUE 1\n")
            out = Path(temp, "generated")
            config.generate(board, out)
            for path, text in ((source, '#include "native.h"\nint value = VALUE;\n'),
                               (header, "#define VALUE 2\n")):
                with self.subTest(source=path.suffix):
                    before = self.snapshot(out)
                    mtime = path.stat().st_mtime_ns
                    path.write_text(text)
                    os.utime(path, ns=(mtime, mtime))
                    config.generate(board, out)
                    self.assert_outputs(out, before, {"sdkconfig.defaults"})
                    before = self.snapshot(out)
                    config.generate(board, out)
                    self.assert_outputs(out, before)

    def test_build_preserves_generated_config_and_repairs_changed_board_json(self):
        for name in ("pico2_w", "esp32s3_8mb"):
            board = config.ROOT / "boards" / f"{name}.json"
            with self.subTest(board=name), tempfile.TemporaryDirectory() as temp, \
                    patch.object(build, "ROOT", Path(temp, "rust")), \
                    patch.object(build.firmware_dependencies, "prepare_btstack"), \
                    patch.object(build, "tool", side_effect=Path), \
                    patch.object(build, "command") as command, \
                    patch.dict(os.environ, CORDIAL_VERSION="1.2.3", IDF_PATH=temp,
                               IDF_PYTHON_ENV_PATH=temp, LIBCLANG_PATH=temp):
                build.build(board, "production", temp, clippy=True)
                path = Path(command.call_args.args[1]["CORDIAL_CONFIG"])
                expected = json.dumps(json.loads(board.read_text()), sort_keys=True).encode()
                self.assertEqual(path.read_bytes(), expected)
                before = self.snapshot(path.parent)
                build.build(board, "production", temp, clippy=True)
                self.assert_outputs(path.parent, before)
                path.write_text("{}")
                before = self.snapshot(path.parent)
                build.build(board, "production", temp, clippy=True)
                self.assert_outputs(path.parent, before, {"board.json"})
                self.assertEqual(path.read_bytes(), expected)
                before = self.snapshot(path.parent)
                changed = Path(temp, "changed.json")
                raw = json.loads(board.read_text()) | {"default_adapter_name": "Desk adapter"}
                changed.write_text(json.dumps(raw))
                build.build(changed, "production", temp, clippy=True)
                selected = Path(command.call_args.args[1]["CORDIAL_CONFIG"])
                self.assertNotEqual(selected.parent, path.parent)
                self.assertEqual(selected.read_bytes(), json.dumps(raw, sort_keys=True).encode())
                self.assert_outputs(path.parent, before)


class BoardConfigTests(unittest.TestCase):
    def test_generic_esp_preset_uses_internal_ram_and_preserves_storage(self):
        board = config.load(config.ROOT / "boards/esp32s3_8mb.json", "production")
        self.assertEqual(board["name"], "esp32s3_8mb")
        self.assertEqual(board["default_adapter_name"], "ESP32-S3")
        self.assertIsNone(board["mcu_led"])
        self.assertEqual(board["flash_bytes"], 8388608)
        self.assertEqual(board["firmware_bytes"], 2097152)
        self.assertEqual(board["profile_memory_budget"], 40960)
        self.assertEqual(board["storage_offset"], 2097152)
        self.assertEqual(board["storage_bytes"], 6291456)
        self.assertEqual(board["storage_identity"].hex(),
                         "7af0b00fd614e5dfc523cba0f6cf2cf871b59483d4b339fe35856d06f1cbaf2a")
        self.assertEqual((board["bluetooth_backend"], board["radio_backend"],
                          board["usb_backend"], board["storage_backend"]),
                         ("esp-nimble", "esp-idf", "embassy", "littlefs"))
        with tempfile.TemporaryDirectory() as out:
            config.generate(board, out)
            generated = Path(out, "board.rs").read_text()
            self.assertIn('DEFAULT_ADAPTER_NAME: &str = "ESP32-S3";', generated)
            self.assertIn('HARDWARE: &str = "esp32s3_8mb";', generated)
            self.assertIn("MCU_LED: Option<(u8, bool)> = None;", generated)

    def test_bluetooth_limits_and_esp_controller_activities(self):
        from bluetooth_config import generate, limits
        for name, expected in (("pico_w", (2, 8)), ("pico2_w", (6, 14)),
                               ("waveshare_rp2350b_plus_w", (6, 14)), ("esp32s3_8mb", (0, 8))):
            path = config.ROOT / "boards" / f"{name}.json"
            board = config.load(path, "development")
            self.assertEqual(limits(board), expected)
            with tempfile.TemporaryDirectory() as out, patch.dict(os.environ, CORDIAL_CONFIG=str(path)):
                generate(out)
                for key, value in zip(("CLASSIC", "BLE", "ACTIVE"), (*expected, sum(expected) + 1)):
                    self.assertIn(f"pub const {key}_CONNECTIONS: usize = {value};\n",
                                  Path(out, "bluetooth.rs").read_text())
                    self.assertIn(f"#define CORDIAL_{key}_CONNECTIONS {value}\n",
                                  Path(out, "cordial_bluetooth.h").read_text())
            if board["chip"] == "esp32s3":
                with tempfile.TemporaryDirectory() as out:
                    config.generate(board, out)
                    sdk = Path(out, "sdkconfig.defaults").read_text()
                    for setting in ("BT_CTRL_BLE_MAX_ACT=10", "BT_NIMBLE_MAX_CONNECTIONS=9",
                                    "BT_NIMBLE_MAX_BONDS=9"):
                        self.assertIn("CONFIG_" + setting + "\n", sdk)

    def test_adapter_defaults_are_baked_without_changing_identity(self):
        for board in config.ROOT.joinpath("boards").glob("*.json"):
            baseline = config.load(board, "production")
            raw = json.loads(board.read_text())
            with tempfile.TemporaryDirectory() as temp:
                path = Path(temp, "board.json")
                path.write_text(json.dumps(raw | {"default_adapter_name": "Desk é"}))
                changed = config.load(path, "production")
                config.generate(changed, temp)
                self.assertIn('DEFAULT_ADAPTER_NAME: &str = "Desk é";', Path(temp, "board.rs").read_text())
                for key in ("hardware_digest", "storage_identity"):
                    self.assertEqual(baseline[key], changed[key])
                for invalid in (None, "", " x ", "x\ny", "é" * 33):
                    path.write_text(json.dumps(raw | {"default_adapter_name": invalid}))
                    with self.assertRaisesRegex(ValueError, "default_adapter_name"):
                        config.load(path, "production")

    def test_profile_memory_budget_is_generated_without_changing_identity(self):
        presets = {"pico_w": None, "pico2_w": 40960, "waveshare_rp2350b_plus_w": 40960,
                   "esp32s3_8mb": 40960}
        for name, expected in presets.items():
            board = config.ROOT / "boards" / f"{name}.json"
            raw = json.loads(board.read_text())
            self.assertEqual(raw["profile_memory_budget"], expected)
            baseline = config.load(board, "production")
            with tempfile.TemporaryDirectory() as temp:
                path = Path(temp, "board.json")
                for budget, constant in ((None, "None"), (1, "Some(1)"), (24576, "Some(24576)"),
                                         (1048575, "Some(1048575)")):
                    path.write_text(json.dumps(raw | {"profile_memory_budget": budget}))
                    changed = config.load(path, "production")
                    out = Path(temp, str(budget))
                    config.generate(changed, out)
                    self.assertIn(f"pub const PROFILE_MEMORY_BUDGET: Option<u32> = {constant};\n",
                                  (out / "board.rs").read_text())
                    for key in ("hardware_digest", "storage_identity"):
                        self.assertEqual(baseline[key], changed[key])
                for invalid in (0, -1, 1048576, 24576.0, "24576", True, [], {}):
                    path.write_text(json.dumps(raw | {"profile_memory_budget": invalid}))
                    with self.assertRaisesRegex(ValueError, "profile_memory_budget"):
                        config.load(path, "production")
                missing = dict(raw)
                del missing["profile_memory_budget"]
                path.write_text(json.dumps(missing))
                with self.assertRaisesRegex(ValueError, "requires exactly"):
                    config.load(path, "production")

    def test_radio_selection_preserves_identity_and_generates_sdk_pins(self):
        preset = json.loads((config.ROOT / "boards/waveshare_rp2350b_plus_w.json").read_text())
        preset["radio"] = {"power": 40, "data": 16, "clock": 47, "cs": 41, "led": None}
        identities = set()
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp, "board.json")
            for backend in ("pico-sdk-cyw43", "embassy-cyw43"):
                path.write_text(json.dumps(preset | {"radio_backend": backend}))
                board = config.load(path, "development")
                identities.add((board["hardware_digest"], board["storage_identity"]))
                out = Path(temp, backend)
                config.generate(board, out)
                if backend == "pico-sdk-cyw43":
                    header = (out / "cordial.h").read_text()
                    self.assertIn("#define PICO_RP2350A 0", header)
                    self.assertIn("#define CYW43_DEFAULT_PIN_WL_CLOCK 47", header)
                    self.assertIn("#define CYW43_DEFAULT_PIN_WL_DATA_IN 16", header)
                else:
                    self.assertFalse((out / "cordial.h").exists())
            self.assertEqual(len(identities), 1)
            path.write_text(json.dumps(preset | {"radio_backend": "esp-idf"}))
            with self.assertRaisesRegex(ValueError, "Pico requires"):
                config.load(path, "development")

    def test_esp_backend_selection_preserves_wiring_and_separates_storage(self):
        preset = json.loads((config.ROOT / "boards/esp32s3_8mb.json").read_text())
        digests, layouts = set(), set()
        choices = {"btstack": "BT_CONTROLLER_ONLY", "esp-nimble": "BT_NIMBLE_ENABLED"}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp, "board.json")
            for backend, choice in choices.items():
                path.write_text(json.dumps(preset | {"bluetooth_backend": backend}))
                board = config.load(path, "development")
                out = Path(temp, backend)
                config.generate(board, out)
                sdk = (out / "sdkconfig.defaults").read_text()
                self.assertIn("CONFIG_SPIRAM=n\n", sdk)
                self.assertIn(f"CONFIG_{choice}=y\n", sdk)
                if backend == "esp-nimble":
                    self.assertIn("CONFIG_BT_NIMBLE_ROLE_PERIPHERAL=y\n", sdk)
                    self.assertIn("CONFIG_BT_NIMBLE_GATT_SERVER=y\n", sdk)
                for other in set(choices.values()) - {choice}:
                    self.assertIn(f"CONFIG_{other}=n\n", sdk)
                self.assertIn("cordial_layout,data,0x40,0x200000,0x1000", (out / "partitions.csv").read_text())
                self.assertIn("cordial_app,data,0x83,0x201000,0x5ff000", (out / "partitions.csv").read_text())
                digests.add(board["hardware_digest"])
                layouts.add(board["storage_identity"])
            self.assertEqual(len(digests), 1)
            self.assertEqual(len(layouts), 1)
            for change in ({"bluetooth_backend": "other"}, {"xosc_hz": 12000000},
                           {"flash_bytes": 3 * 1048576}, {"mcu_led": {"pin":19,"active_low":True}},
                           {"mcu_led": {"pin":33,"active_low":False}}, {"radio": {}}):
                path.write_text(json.dumps(preset | change))
                with self.assertRaises(ValueError):
                    config.load(path, "development")

    def test_presets_and_custom_pins(self):
        for name, feature in (("pico_w", "rp2040"), ("pico2_w", "rp235xa"),
                              ("waveshare_rp2350b_plus_w", "rp235xb")):
            board = config.load(config.ROOT / "boards" / f"{name}.json", "development")
            self.assertEqual(board["feature"], feature)
            self.assertEqual(board["storage_offset"] % 4096, 0)
            with tempfile.TemporaryDirectory() as out:
                config.generate(board, out)
                generated = Path(out, "board.rs").read_text()
                self.assertIn(f"$p.PIN_{board['radio']['data']}", generated)
                self.assertIn("LENGTH = 65536", Path(out, "memory.x").read_text())
        pico = config.load(config.ROOT / "boards/pico_w.json", "production")
        self.assertEqual(pico["hardware_digest"], "22410797341ffa56e16b6cc87598d2c23820dacd176b988d38dc8b73f4583d30")
        raw = json.loads((config.ROOT / "boards/waveshare_rp2350b_plus_w.json").read_text())
        raw["name"] = "custom_rp2350"
        raw["radio"] = {"power": 40, "data": 16, "clock": 47, "cs": 41, "led": None}
        with tempfile.TemporaryDirectory() as out:
            path = Path(out, "board.json")
            path.write_text(json.dumps(raw))
            custom = config.load(path, "development")
            config.generate(custom, out)
            self.assertIn("$p.PIN_47", Path(out, "board.rs").read_text())
            self.assertIn("RADIO_LED: Option<(u8, bool)> = None", Path(out, "board.rs").read_text())
            self.assertIn("$p.PIN_23", Path(out, "board.rs").read_text())
            for change in ({"package": "a"}, {"bluetooth_backend": "esp-nimble"},
                           {"firmware_bytes": 32768}, {"xosc_hz": 13000000},
                           {"xosc_hz": 6250000}, {"xosc_hz": 7500000},
                           {"xosc_hz": 9375000}, {"xosc_hz": 12500000},
                           {"radio": raw["radio"] | {"data": 15}},
                           {"mcu_led": 23}, {"mcu_led": {"pin": 40, "active_low": False}},
                           {"mcu_led": {"pin": 23, "active_low": 1}},
                           {"radio": raw["radio"] | {"cs": 40}}, {"typo": 1}):
                path.write_text(json.dumps(copy.deepcopy(raw) | change))
                with self.assertRaises(ValueError):
                    config.load(path, "development")


if __name__ == "__main__":
    unittest.main()
