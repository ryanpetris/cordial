"""Current-format artifact round trips and storage/profile validation, offline."""
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import firmware_artifact as artifact
import firmware_config
from flash_dev import inspect_uf2


class Artifact(unittest.TestCase):
    def test_pico_presets_pack_both_profiles_and_preserve_reserved_bounds(self):
        for board in ('pico_w', 'pico2_w', 'waveshare_rp2350b_plus_w'):
            for profile in ('development', 'production'):
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
                        {'storage_bytes': 4096}, {'native_storage_bytes': 0x7000},
                        {'profile': 'production'}, {'unused': True}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                artifact.inspect(blob(expected | changes))


if __name__ == '__main__':
    unittest.main()
