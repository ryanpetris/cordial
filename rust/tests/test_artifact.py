"""Current-format artifact round trips and storage/profile validation, offline."""
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import firmware_artifact as artifact
import firmware_config
from flash_dev import inspect_uf2
from build_firmware import esp_startup_frames


class EspStartupStack(unittest.TestCase):
    SMALL = """
4203636c <cordial_esp32s3::main>:
4203636c: 004136         entry a1, 32
4203636f: 2cdd81         l32r a8, 420016e4
42036372: 0008e0         callx8 a8
42036378 <cordial_esp32s3::start>:
42036378: 2a0136         entry a1, 0x1500
4203637b: 080c           movi.n a8, 0
"""

    def test_inlined_construction_fits_and_unrelated_frames_are_ignored(self):
        frames = esp_startup_frames(self.SMALL + """
420679e8 <unrelated_function>:
420679e8: 004136         entry a1, 32
420679f1: 001810         movsp a1, a8
""")
        self.assertEqual(sum(frames.values()), 5408)

    def test_extended_constructor_frame_is_rejected(self):
        # A constructor can allocate a large frame even when its caller stays small.
        with self.assertRaisesRegex(ValueError, "Extended ESP startup"):
            esp_startup_frames(self.SMALL + """
420679e8 <<cordial_core::application::Application>::new>:
420679e8: 004136         entry a1, 32
420679eb: 6ced81         l32r a8, 42042da0 <literal> (98d0 <size>)
420679ee: c08180         sub a8, a1, a8
420679f1: 001810         movsp a1, a8
""")

    def test_nested_frames_share_one_budget(self):
        with self.assertRaisesRegex(ValueError, "exceed their budget"):
            esp_startup_frames(self.SMALL + """
420679e8 <<cordial_core::application::Application>::new>:
420679e8: 600136         entry a1, 0x3000
""")

    def test_main_cannot_retain_a_large_frame(self):
        with self.assertRaisesRegex(ValueError, "exceed their budget"):
            esp_startup_frames(self.SMALL.replace("entry a1, 32", "entry a1, 2048"))

    def test_missing_or_unrecognized_prologues_fail(self):
        for listing in ("", self.SMALL.replace("entry a1, 32", "nop.n"),
                        self.SMALL[:self.SMALL.index("42036378:")]):
            with self.subTest(listing=listing), self.assertRaises(ValueError):
                esp_startup_frames(listing)


class Artifact(unittest.TestCase):
    def test_pico_presets_pack_every_profile_and_preserve_reserved_bounds(self):
        for board in ('pico_w', 'pico2_w', 'waveshare_rp2350b_plus_w'):
            for profile in artifact.PROFILES:
                config = firmware_config.load(ROOT / 'boards' / (board + '.json'), profile)
                expected = artifact.manifest(config)
                image = b'\0' * 512 + artifact.MAGIC + json.dumps(expected).encode() + b'\0'
                self.assertEqual(inspect_uf2(artifact.uf2(image)), expected)
                with self.assertRaisesRegex(ValueError, 'overlaps'):
                    artifact.uf2(image + b'\0' * config['storage_offset'])

    def test_backend_identity_and_bootloader_capability_are_checked(self):
        config = firmware_config.load(ROOT / 'boards/xiao_esp32s3.json', 'development')
        expected = artifact.manifest(config)
        def blob(value):
            return artifact.MAGIC + json.dumps(value).encode() + b'\0'
        self.assertEqual(artifact.inspect(blob(expected)), expected)
        for changes in ({'bootloader': 'bootsel'},
                        {'storage_bytes': 4096}, {'storage_identity': '0' * 64},
                        {'profile': 'production'}, {'unused': True}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                artifact.inspect(blob(expected | changes))

    def test_profile_selects_version_suffix_and_bootloader(self):
        expected = {'production': ('1.2.3', None), 'debug': ('1.2.3-debug', 'bootsel'),
                    'development': ('1.2.3-dev', 'bootsel')}
        with patch.dict(os.environ, CORDIAL_VERSION='1.2.3'):
            for profile, (version, bootloader) in expected.items():
                with self.subTest(profile=profile):
                    value = artifact.manifest(firmware_config.load(ROOT / 'boards/pico_w.json', profile))
                    self.assertEqual((value['version'], value['bootloader']), (version, bootloader))

    def test_version_accepts_only_profile_suffixes(self):
        config = firmware_config.load(ROOT / 'boards/pico_w.json', 'production')
        def blob(value):
            return artifact.MAGIC + json.dumps(artifact.manifest(config) | {'version': value}).encode() + b'\0'
        for value in ('1.2.3', '1.2.3-debug', '1.2.3-dev'):
            self.assertEqual(artifact.inspect(blob(value))['version'], value)
        for value in ('1.2.3-rc1', '1.2.3-debug-dev', 'v1.2.3'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                artifact.inspect(blob(value))


if __name__ == '__main__':
    unittest.main()
