"""Regression checks for bootstrap recovery, tool selection and diagnostics."""
import hashlib
from contextlib import nullcontext
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import bootstrap as common


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


desktop = load('desktop_bootstrap', common.ROOT / 'desktop/bootstrap.py')
rust = load('rust_bootstrap', common.ROOT / 'rust/bootstrap.py')


class BootstrapTests(unittest.TestCase):
    def test_package_diagnostics(self):
        for distro, expected in [('arch', 'Missing packages: gcc git'),
                                 ('debian', 'Missing packages: build-essential git')]:
            with patch.object(common.platform, 'freedesktop_os_release', return_value={'ID': distro}), \
                 patch.object(common.shutil, 'which', return_value=None):
                with self.assertRaisesRegex(ValueError, expected):
                    common.require([('cc', 'gcc', 'build-essential'), ('git', 'git', 'git')])

    def test_download_rejects_bad_checksum_and_recovers(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(common, 'CACHE', Path(tmp)), \
             patch.object(common.urllib.request, 'urlopen', side_effect=lambda *a, **k: io.BytesIO(b'good')) as fetch:
            with self.assertRaisesRegex(ValueError, 'Checksum mismatch'):
                common.download('https://invalid.test/tool', '0' * 64, 'tool')
            self.assertEqual(list(Path(tmp).iterdir()), [])
            digest = hashlib.sha256(b'good').hexdigest()
            path = common.download('https://invalid.test/tool', digest, 'tool')
            common.download('https://invalid.test/tool', digest, 'tool')
            self.assertEqual(fetch.call_count, 2)
            path.write_bytes(b'truncated')
            common.download('https://invalid.test/tool', digest, 'tool')
            self.assertEqual(path.read_bytes(), b'good')

    def test_archive_cannot_escape_installation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / 'bad.tar'
            with tarfile.open(archive, 'w') as tar:
                member = tarfile.TarInfo('../escaped')
                member.size = 1
                tar.addfile(member, io.BytesIO(b'x'))
            with self.assertRaises(tarfile.FilterError):
                common.unpack(archive, root / 'tools')
            self.assertFalse((root / 'escaped').exists())
            self.assertFalse((root / 'tools').exists())

    def test_npm_install_recovers_and_tracks_inputs(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(desktop, 'ROOT', Path(tmp)), \
             patch.object(common, 'output', return_value='24.19.0'), \
             patch.object(common.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as check:
            root = Path(tmp)
            (root / 'package.json').write_text('{}')
            (root / 'package-lock.json').write_text('{}')
            stamp = root / 'node_modules/.cordial-bootstrap'
            def install(*args, **kwargs):
                stamp.parent.mkdir(exist_ok=True)
            with patch.object(common, 'run', side_effect=install) as ci:
                desktop.dependencies({})
                desktop.dependencies({})
                self.assertEqual(ci.call_count, 1)
                self.assertIn('--include=dev', ci.call_args.args[0])
                (root / 'package-lock.json').write_text('{"changed":true}')
                desktop.dependencies({})
                self.assertEqual(ci.call_count, 2)
                check.return_value = subprocess.CompletedProcess([], 1)
                desktop.dependencies({})
                self.assertEqual(ci.call_count, 3)
                ci.side_effect = subprocess.CalledProcessError(1, 'npm ci')
                with self.assertRaises(subprocess.CalledProcessError):
                    desktop.dependencies({})
                self.assertFalse(stamp.exists())

    def test_rustup_overrides_system_rust(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(common, 'TOOLS', Path(tmp) / 'tools'):
            root = Path(tmp)
            rustup = root / 'rustup'
            rustup.write_text('#!/bin/sh\nexit 0\n')
            rustup.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + ':/usr/bin')
            with patch.object(common, 'output', return_value=''), patch.object(common, 'run') as install:
                rust.rust(env)
            proxy = Path(env['PATH'].split(':')[0]) / 'cargo'
            self.assertEqual(proxy.resolve(), rustup)
            self.assertEqual(env['RUSTUP_TOOLCHAIN'], rust.tomllib.loads(
                (rust.ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel'])
            install.assert_not_called()
            self.assertNotIn('RUSTUP_HOME', {k:v for k,v in env.items() if k not in os.environ})

    def test_arch_packaging_preflight(self):
        for distro, expected in [('arch', 'Missing packages: libarchive'),
                                 ('debian', 'Missing packages: libarchive-tools')]:
            with patch.object(sys, 'argv', ['bootstrap.py', 'package:arch']), \
                 patch.object(common.platform, 'freedesktop_os_release', return_value={'ID': distro}), \
                 patch.object(common.shutil, 'which', side_effect=lambda name: None if name == 'bsdtar' else '/usr/bin/' + name):
                with self.assertRaisesRegex(ValueError, expected):
                    desktop.main()

    def test_rustup_outside_path(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(common, 'TOOLS', Path(tmp) / 'tools'):
            cargo_home = Path(tmp) / 'cargo'
            (cargo_home / 'bin').mkdir(parents=True)
            rustup = cargo_home / 'bin/rustup'
            rustup.write_text('#!/bin/sh\nexit 0\n')
            rustup.chmod(0o755)
            env = dict(os.environ, CARGO_HOME=str(cargo_home))
            with patch.object(common.shutil, 'which', return_value=None), \
                 patch.object(common, 'output', return_value=''), patch.object(common, 'download') as download:
                rust.rust(env)
            self.assertEqual(env['CARGO_HOME'], str(cargo_home))
            self.assertEqual((common.TOOLS / 'rust-bin/cargo').resolve(), rustup)
            download.assert_not_called()

    def test_newlib_notice_layouts(self):
        from dependency_notices import newlib_notice
        for relative in ('share/licenses/arm-none-eabi-newlib/COPYING.NEWLIB',
                         'share/doc/libnewlib-arm-none-eabi/copyright', 'license.txt'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                notice = root / relative
                notice.parent.mkdir(parents=True, exist_ok=True)
                notice.write_text('newlib license')
                self.assertEqual(newlib_notice(root / 'bin/arm-none-eabi-gcc'), notice)

    def test_missing_clang_preflight(self):
        with patch.object(sys, 'argv', ['bootstrap.py', 'firmware']), \
             patch.object(common.platform, 'freedesktop_os_release', return_value={'ID': 'debian'}), \
             patch.object(common.shutil, 'which', side_effect=lambda name: None if name == 'clang' else '/usr/bin/' + name):
            with self.assertRaisesRegex(ValueError, 'Missing packages: clang'):
                rust.main()

    def test_missing_libclang_diagnostic(self):
        with patch.object(Path, 'glob', return_value=[]), \
             patch.object(common.platform, 'freedesktop_os_release', return_value={'ID': 'debian'}):
            with self.assertRaisesRegex(ValueError, 'Missing packages: libclang-dev'):
                rust.libclang({})

    def test_npm_argument_forwarding(self):
        for extra in (['--arch'], ['--', '--arch']):
            with patch.object(sys, 'argv', ['bootstrap.py', 'dist', *extra]), \
                 patch.object(common, 'require'), patch.object(common, 'lock', return_value=nullcontext()), \
                 patch.object(desktop, 'node'), patch.object(desktop, 'dependencies'), \
                 patch.object(common, 'run') as run:
                desktop.main()
                self.assertEqual(run.call_args.args[0], ['npm', 'run', 'dist', '--', '--arch'])

    def test_supported_node_versions(self):
        for version in ('v22.12.0', 'v24.19.0', 'v26.9.0'):
            self.assertTrue(desktop.supported(version))
        for version in ('v20.19.0', 'v22.11.0', 'v23.0.0', 'v25.0.0', 'v24.19.0-nightly', 'v26.0.0-rc.1'):
            self.assertFalse(desktop.supported(version))

    def test_parallel_install_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            code = '''import sys,time
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import bootstrap
bootstrap.CACHE=Path(sys.argv[2])
with bootstrap.lock('test'):
 p=bootstrap.CACHE/'events'
 with p.open('a') as f: f.write('start\\n')
 time.sleep(.1)
 with p.open('a') as f: f.write('end\\n')
'''
            jobs = [subprocess.Popen([sys.executable, '-c', code, str(common.ROOT/'tools'), tmp]) for _ in range(3)]
            for job in jobs:
                self.assertEqual(job.wait(timeout=10), 0)
            self.assertEqual((Path(tmp)/'events').read_text(), 'start\nend\n'*3)


if __name__ == '__main__':
    unittest.main()
