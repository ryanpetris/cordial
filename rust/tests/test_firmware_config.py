import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import firmware_config as config


class BoardConfigTests(unittest.TestCase):
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
        preset = json.loads((config.ROOT / "boards/xiao_esp32s3.json").read_text())
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
