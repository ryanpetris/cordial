"""Exercise release compatibility policies with Git snapshots and real Buf comparisons."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from check_protocol import check, version_storage_schema
from version import protocol_base, released_tags

ROOT = Path(__file__).resolve().parents[1]
BUF = os.environ.get('BUF', 'buf')


class ProtocolBaselineTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        config = self.root / 'buf.yaml'
        config.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / 'buf.yaml', config)
        self.schemas = {
            'shared/common.proto': 'syntax = "proto3"; package fixture.common; message Value { uint32 value = 1; }',
            'handshake.proto': 'syntax = "proto3"; package fixture.handshake; import "shared/common.proto"; '
                               'message ClientHello { optional uint32 protocol_version = 1; fixture.common.Value info = 2; }',
            'serial/v1/cordial.proto': 'syntax = "proto3"; package fixture.serial.v1; import "shared/common.proto"; '
                                      'message Request { uint32 id = 1; fixture.common.Value value = 2; } '
                                      'enum State { STATE_UNSPECIFIED = 0; STATE_CONNECTED = 1; }',
            'storage/v2/storage.proto': 'syntax = "proto3"; package fixture.storage; message Saved { uint32 value = 1; }',
            'migration.proto': 'syntax = "proto3"; package fixture.migration; message Start { string adapter_id = 1; }',
        }
        for path, text in self.schemas.items():
            self.write(path, text)
        self.git('add', '.')
        self.git('commit', '-qm', 'Protocol fixture')
        self.git('tag', 'v0.9.0')
        self.git('commit', '--allow-empty', '-qm', 'Development fixture')
        self.environment = patch.dict(os.environ, CORDIAL_VERSION='0.10.0')
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True, capture_output=True)

    def write(self, path, text):
        destination = self.root / 'proto' / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)

    def test_new_series_check_handshake_with_empty_storage_base(self):
        self.write('handshake.proto', self.schemas['handshake.proto'].replace('uint32 protocol_version', 'string protocol_version'))
        for version in ['0.10.0', '1.0.0', '2.0.0']:
            with self.subTest(version=version), patch.dict(os.environ, CORDIAL_VERSION=version):
                with self.assertRaises(subprocess.CalledProcessError):
                    check('', self.root, BUF)

    def test_noncurrent_released_revision_stays_protected(self):
        self.write('serial/v2/cordial.proto', 'syntax = "proto3"; package fixture.serial.v2; message Request { string id = 1; }')
        self.write('serial/v1/cordial.proto', self.schemas['serial/v1/cordial.proto'].replace('uint32 id', 'string id'))
        with self.assertRaises(subprocess.CalledProcessError):
            check('', self.root, BUF)

    def test_deleted_released_revision_is_an_error(self):
        shutil.rmtree(self.root / 'proto/serial/v1')
        self.write('serial/v2/cordial.proto', 'syntax = "proto3"; package fixture.serial.v2; message Request {}')
        with self.assertRaisesRegex(ValueError, 'Released protocol contract is missing: proto/serial/v1'):
            check('', self.root, BUF)

    def test_released_messages_and_enums_cannot_disappear(self):
        for path, source in [
            ('handshake.proto', 'syntax = "proto3"; package fixture.handshake;'),
            ('serial/v1/cordial.proto', 'syntax = "proto3"; package fixture.serial.v1;'),
            ('serial/v1/cordial.proto', self.schemas['serial/v1/cordial.proto'].split('enum State')[0]),
        ]:
            with self.subTest(path=path, source=source):
                self.write(path, source)
                with self.assertRaisesRegex(ValueError, 'Released protocol definitions are missing:'):
                    check('', self.root, BUF)
                self.write(path, self.schemas[path])

    def test_nested_definitions_cannot_disappear(self):
        source = self.schemas['serial/v1/cordial.proto'] + (
            ' message Container { message Nested {} enum Mode { MODE_UNSPECIFIED = 0; } }')
        self.write('serial/v1/cordial.proto', source)
        self.git('add', '.')
        self.git('commit', '-qm', 'Nested definitions fixture')
        self.git('tag', 'v0.9.1')
        for definition, name in [
            ('message Nested {}', 'message fixture.serial.v1.Container.Nested'),
            ('enum Mode { MODE_UNSPECIFIED = 0; }', 'enum fixture.serial.v1.Container.Mode'),
        ]:
            with self.subTest(name=name):
                self.write('serial/v1/cordial.proto', source.replace(definition, ''))
                with self.assertRaises(ValueError) as error:
                    check('', self.root, BUF)
                self.assertIn(name, str(error.exception))

    def test_definitions_can_move_between_files_in_the_same_package(self):
        self.write('hello.proto', self.schemas['handshake.proto'])
        self.write('handshake.proto', 'syntax = "proto3"; package fixture.handshake;')
        (self.root / 'proto/serial/v1/cordial.proto').unlink()
        source, enum = self.schemas['serial/v1/cordial.proto'].split('enum State')
        self.write('serial/v1/messages.proto', source)
        self.write('serial/v1/types/state.proto',
                   'syntax = "proto3"; package fixture.serial.v1; enum State' + enum)
        check('', self.root, BUF)
        self.write('serial/v1/messages.proto', source.replace('uint32 id', 'string id'))
        with self.assertRaises(subprocess.CalledProcessError):
            check('', self.root, BUF)
        self.write('serial/v1/messages.proto', source)
        self.write('serial/v1/types/state.proto', 'syntax = "proto3"; package fixture.serial.v1;')
        with self.assertRaisesRegex(ValueError, 'enum fixture.serial.v1.State'):
            check('', self.root, BUF)

    def test_dependencies_are_available_and_protected(self):
        check('', self.root, BUF)
        self.write('shared/common.proto', self.schemas['shared/common.proto'].replace('uint32 value', 'string value'))
        with self.assertRaises(subprocess.CalledProcessError):
            check('', self.root, BUF)

    def test_storage_uses_its_own_baseline(self):
        self.write('storage/v2/storage.proto', self.schemas['storage/v2/storage.proto'].replace('uint32 value', 'string value'))
        check('', self.root, BUF)
        with self.assertRaises(subprocess.CalledProcessError):
            check('v0.9.0', self.root, BUF)
        self.write('storage/v2/storage.proto', 'syntax = "proto3"; package fixture.storage;')
        check('v0.9.0', self.root, BUF)

    def test_migration_ipc_uses_the_release_series_baseline(self):
        self.write('migration.proto', self.schemas['migration.proto'].replace('string adapter_id', 'bytes adapter_id'))
        check('', self.root, BUF)
        with self.assertRaises(subprocess.CalledProcessError):
            check('v0.9.0', self.root, BUF)

    def test_each_retained_storage_format_is_protected(self):
        self.write('storage/v1/storage.proto',
                   'syntax = "proto3"; package fixture.storage.v1; message Saved { uint32 value = 1; }')
        self.git('add', '.')
        self.git('commit', '-qm', 'Historical storage fixture')
        self.git('tag', 'v0.9.1')
        self.write('storage/v1/storage.proto',
                   'syntax = "proto3"; package fixture.storage.v1; message Saved { string value = 1; }')
        with self.assertRaises(subprocess.CalledProcessError):
            check('v0.9.1', self.root, BUF)

    def test_unversioned_storage_is_compared_in_its_existing_namespace(self):
        for version in ['v1', 'v2']:
            with self.subTest(version=version):
                shutil.rmtree(self.root / 'proto/storage')
                text = (ROOT / f'proto/storage/{version}/storage.proto').read_text()
                old = text.replace(f'package cordial.storage.{version};', 'package cordial.storage;')
                path, normalized = version_storage_schema(old)
                self.assertEqual(path, f'proto/storage/{version}/storage.proto')
                self.assertEqual(normalized, text)
                self.write('storage.proto', old)
                self.git('add', '.')
                self.git('commit', '-qm', 'Unversioned storage fixture')
                base = self.git('rev-parse', 'HEAD').stdout.decode().strip()
                (self.root / 'proto/storage.proto').unlink()
                self.write(f'storage/{version}/storage.proto', text)
                check(base, self.root, BUF)
                self.write(f'storage/{version}/storage.proto', text.replace('uint64 next_device', 'string next_device'))
                with self.assertRaises(subprocess.CalledProcessError):
                    check(base, self.root, BUF)
                self.write(f'storage/{version}/storage.proto', text)

    def publish_other_branch(self):
        self.git('switch', '-qc', 'published')
        for path in ['handshake.proto', 'serial/v1/cordial.proto']:
            self.write(path, self.schemas[path].replace(' }', ' optional bool published = 3; }', 1))
        self.write('serial/v2/cordial.proto', 'syntax = "proto3"; package fixture.serial.v2; message Request { string id = 1; }')
        self.write('storage/v2/storage.proto', self.schemas['storage/v2/storage.proto'].replace('uint32 value', 'string value'))
        self.git('add', '.')
        self.git('commit', '-qm', 'Published contracts fixture')
        self.git('tag', 'v0.10.0')
        self.git('switch', '-qc', 'maintenance', 'v0.9.0')
        self.git('commit', '--allow-empty', '-qm', 'Maintenance fixture')

    def test_nonancestor_higher_release_protects_handshake_and_shared_revision(self):
        self.publish_other_branch()
        compatible = {
            path: self.schemas[path].replace(' }', ' optional bool published = 3; }', 1)
            for path in ['handshake.proto', 'serial/v1/cordial.proto']
        }
        for path, text in compatible.items():
            self.write(path, text)
        with patch.dict(os.environ, CORDIAL_VERSION='0.9.1'):
            check('', self.root, BUF)
            for path, text in compatible.items():
                with self.subTest(path=path):
                    self.write(path, text.replace('bool published', 'string published'))
                    with self.assertRaises(subprocess.CalledProcessError):
                        check('', self.root, BUF)
                    self.write(path, text)

    def test_nonancestor_future_revision_is_optional_and_storage_stays_same_series(self):
        self.publish_other_branch()
        with patch.dict(os.environ, CORDIAL_VERSION='0.9.1'):
            base = protocol_base('0.9.1', released_tags(self.root))
            self.assertEqual(base, 'v0.9.0')
            for path in ['handshake.proto', 'serial/v1/cordial.proto']:
                self.write(path, self.schemas[path].replace(' }', ' optional bool published = 3; }', 1))
            self.assertFalse((self.root / 'proto/serial/v2').exists())
            check(base, self.root, BUF)
            self.write('storage/v2/storage.proto', self.schemas['storage/v2/storage.proto'].replace('uint32 value', 'string value'))
            with self.assertRaises(subprocess.CalledProcessError):
                check(base, self.root, BUF)

    def test_every_published_release_is_compared(self):
        self.publish_other_branch()
        self.git('switch', '-q', 'published')
        for path in ['handshake.proto', 'serial/v1/cordial.proto']:
            self.write(path, self.schemas[path])
        self.git('add', '.')
        self.git('commit', '-qm', 'Independent release fixture')
        self.git('tag', 'v0.11.0')
        self.git('switch', '-q', 'maintenance')
        with patch.dict(os.environ, CORDIAL_VERSION='0.9.1'):
            with self.assertRaises(subprocess.CalledProcessError):
                check('', self.root, BUF)

    def test_release_being_built_is_excluded_even_when_tagged_at_head(self):
        self.write('handshake.proto', self.schemas['handshake.proto'].replace(' }', ' optional bool unpublished = 3; }'))
        self.git('add', '.')
        self.git('commit', '-qm', 'Current release fixture')
        self.git('tag', 'v0.9.1')
        self.write('handshake.proto', self.schemas['handshake.proto'])
        with patch.dict(os.environ, CORDIAL_VERSION='0.9.1'):
            check('', self.root, BUF)

    def test_unversioned_release_only_compares_storage(self):
        shutil.rmtree(self.root / 'proto/serial')
        (self.root / 'proto/handshake.proto').unlink()
        self.write('cordial.proto', 'syntax = "proto3"; package fixture.legacy; message Request {}')
        self.git('add', '.')
        self.git('commit', '-qm', 'Unversioned fixture')
        self.git('tag', 'v0.10.0')
        self.git('commit', '--allow-empty', '-qm', 'Development fixture')
        (self.root / 'proto/cordial.proto').unlink()
        for path, text in self.schemas.items():
            self.write(path, text)
        with patch.dict(os.environ, CORDIAL_VERSION='0.10.1'):
            check('v0.10.0', self.root, BUF)
            self.write('storage/v2/storage.proto', self.schemas['storage/v2/storage.proto'].replace('uint32 value', 'string value'))
            with self.assertRaises(subprocess.CalledProcessError):
                check('v0.10.0', self.root, BUF)


if __name__ == '__main__':
    unittest.main()
