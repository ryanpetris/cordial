#!/usr/bin/env python3
"""Compare permanent serial contracts across releases and storage/IPC within a release series."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from version import released_tags, resolve, stable_tags


def stable_schema(path):
    return path == 'proto/handshake.proto' or bool(
        re.fullmatch(r'proto/serial/v[1-9][0-9]*/.+\.proto', path))


def definitions(image):
    names = set()

    def collect(scope, prefix, message_key):
        for kind, key in [('message', message_key), ('enum', 'enumType')]:
            for definition in scope.get(key, []):
                name = prefix + definition['name']
                names.add((kind, name))
                if kind == 'message':
                    collect(definition, name + '.', 'nestedType')

    for file in json.loads(image.read_text())['file']:
        package = file.get('package', '')
        collect(file, package + '.' if package else '', 'messageType')
    return names


def version_storage_schema(text):
    """Map an unversioned storage namespace to its existing format without changing fields."""
    identity = re.search(r'message Identity\s*\{([\s\S]*?)\n\}', text)
    if not identity:
        raise ValueError('Historical storage schema has no Identity record')
    identity = identity[1]
    if re.search(r'\buint32 format = 1;', identity) and re.search(r'\bbytes address = 2;', identity):
        version = 'v2'
    elif not re.search(r'\bformat\s*=', identity) and re.search(r'\bbytes address = 1;', identity):
        version = 'v1'
    else:
        raise ValueError('Historical storage schema has an unknown format')
    if 'package cordial.storage;' not in text:
        raise ValueError('Unexpected historical storage package')
    return (f'proto/storage/{version}/storage.proto',
            text.replace('package cordial.storage;', f'package cordial.storage.{version};'))


def compare(base, root, buf, *, stable):
    released = subprocess.check_output(
        ['git', 'ls-tree', '-r', '--name-only', base, '--', 'proto'], cwd=root, text=True,
    ).splitlines()
    selected = [path for path in released if stable_schema(path)] if stable else [
        path for path in released if path in ('proto/storage.proto', 'proto/migration.proto')
        or path.startswith('proto/storage/') and path.endswith('.proto')]
    if not selected:
        return
    if stable:
        ancestor = subprocess.run(
            ['git', 'merge-base', '--is-ancestor', base, 'HEAD'], cwd=root, check=False,
        )
        if ancestor.returncode not in (0, 1):
            ancestor.check_returncode()
        present = []
        for path in selected:
            contract = root / ('proto/handshake.proto' if path == 'proto/handshake.proto'
                               else '/'.join(path.split('/')[:3]))
            if not contract.exists() or contract.is_dir() and not any(contract.rglob('*.proto')):
                if ancestor.returncode == 0 or path == 'proto/handshake.proto':
                    raise ValueError(f'Released protocol contract is missing: {contract.relative_to(root)}')
            else:
                present.append(path)
        selected = present
        if not selected:
            return
    config = root / 'buf.yaml'
    with tempfile.TemporaryDirectory(prefix='cordial-protocol-base-') as temporary:
        snapshot = Path(temporary) / 'proto'
        # Imports may live outside the selected contract. Build the baseline image with its
        # complete source tree, then select only the contract and its transitive dependencies.
        for path in released:
            if not path.endswith('.proto'):
                continue
            text = subprocess.check_output(['git', 'show', f'{base}:{path}'], cwd=root, text=True)
            if not stable and path == 'proto/storage.proto':
                normalized, text = version_storage_schema(text)
                selected = [normalized if entry == path else entry for entry in selected]
                path = normalized
            destination = snapshot / Path(path).relative_to('proto')
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(text)
        image = Path(temporary) / 'baseline.json'
        paths = [argument for path in selected for argument in (
            '--path', str(snapshot / Path(path).relative_to('proto')))]
        subprocess.run([str(buf), 'build', str(snapshot), '--config', str(config),
                        *paths, '-o', str(image)], cwd=root, check=True)
        subprocess.run([str(buf), 'breaking', str(root / 'proto'), '--config',
                        str(config), '--against-config', str(config),
                        '--against', str(image)], cwd=root, check=True)
        if stable:
            current = Path(temporary) / 'current.json'
            subprocess.run([str(buf), 'build', str(root / 'proto'), '--config', str(config),
                            '-o', str(current)], cwd=root, check=True)
            # Buf's package deletion rules skip the last message or enum in a package.
            missing = definitions(image) - definitions(current)
            if missing:
                raise ValueError('Released protocol definitions are missing: ' + ', '.join(
                    f'{kind} {name}' for kind, name in sorted(missing)))


def check(base, root, buf, stable_base=None):
    baselines = [stable_base] if stable_base else stable_tags(
        resolve(), released_tags(root, ancestors_only=False))
    for baseline in baselines:
        compare(baseline, root, buf, stable=True)
    if base:
        compare(base, root, buf, stable=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True, help='same-series storage and migration IPC baseline, or empty')
    parser.add_argument('--stable-base', help='override all local serial release baselines with one Git revision')
    parser.add_argument('--buf', default=os.environ.get('BUF', 'buf'), help='native Buf executable')
    args = parser.parse_args()
    check(args.base, Path(__file__).resolve().parents[1], args.buf, args.stable_base)


if __name__ == '__main__':
    main()
