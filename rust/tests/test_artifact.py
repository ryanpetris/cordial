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
                        {'storage_bytes': 4096}, {'native_storage_bytes': 0x7000},
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
