#!/usr/bin/env python3
"""Install desktop build dependencies, then run the requested npm script."""
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
import bootstrap as common

ROOT = common.ROOT / 'desktop'
NODE_VERSION = '24.19.0'
NODE_HASHES = {
    'x86_64': '14b342e71204f811bde6153be8e04b62aef63c236fef92b55f9c83154b409647',
    'aarch64': '01443c1e1a29e531ccad5a46fefa6df490d2189c49f7955904aecdbb0fe86fdc',
}


def supported(version):
    match = re.fullmatch(r'v?(\d+)\.(\d+)\.(\d+)', version)
    if not match:
        return False
    parts = tuple(map(int, match.groups()))
    return parts >= (22, 12, 0) and (parts[0] == 22 or parts[0] == 24 or parts[0] >= 26)


def node(env):
    if shutil.which('node', path=env['PATH']) and shutil.which('npm', path=env['PATH']):
        if supported(common.output(['node', '--version'], env=env)):
            return
    machine = common.host()
    arch = {'x86_64': 'x64', 'aarch64': 'arm64'}[machine]
    name = f'node-v{NODE_VERSION}-linux-{arch}'
    destination = common.TOOLS / name
    if not (destination / 'bin/node').is_file() or not (destination / 'bin/npm').is_file():
        archive = common.download(f'https://nodejs.org/dist/v{NODE_VERSION}/{name}.tar.xz',
                                  NODE_HASHES[machine], name + '.tar.xz')
        common.unpack(archive, destination)
    common.prepend(env, destination / 'bin')


def fingerprint(env):
    digest = hashlib.sha256()
    for name in ('package.json', 'package-lock.json'):
        digest.update((ROOT / name).read_bytes())
    for command in (['node', '--version'], ['npm', '--version']):
        digest.update(common.output(command, env=env).encode())
    digest.update(common.host().encode())
    return digest.hexdigest()


def dependencies(env):
    stamp = ROOT / 'node_modules/.cordial-bootstrap'
    expected = fingerprint(env)
    # npm validates transitive packages as well as the top-level dependencies.
    installed = stamp.exists() and stamp.read_text() == expected
    if installed:
        result = common.subprocess.run(['npm', 'ls', '--all', '--include=dev', '--json'], cwd=ROOT, env=env,
                                       stdout=common.subprocess.DEVNULL, stderr=common.subprocess.DEVNULL)
        installed = result.returncode == 0
    if not installed:
        stamp.unlink(missing_ok=True)
        common.run(['npm', 'ci', '--include=dev', '--no-audit', '--no-fund'], cwd=ROOT, env=env)
        stamp.write_text(expected)


def main():
    script = sys.argv[1] if len(sys.argv) > 1 else 'build'
    extra = sys.argv[2:]
    if extra[:1] == ['--']:
        extra = extra[1:]
    if script not in json.loads((ROOT / 'package.json').read_text())['scripts']:
        raise ValueError('Unknown desktop script: ' + script)
    commands = common.NATIVE + [('pkg-config', 'pkgconf', 'pkg-config')]
    if script == 'package:arch' or (script == 'dist' and '--arch' in extra):
        commands += [('bsdtar', 'libarchive', 'libarchive-tools')]
    common.require(commands)
    env = dict(os.environ, CORDIAL_PYTHON=sys.executable)
    with common.lock('desktop'):
        node(env)
        dependencies(env)
        # Match package.json's launch scripts, but hold the lock only for the build.
        if script in ('start', 'simulate'):
            common.run(['npm', 'run', 'build'], cwd=ROOT, env=env)
        elif script != 'serve':
            common.run(['npm', 'run', script, '--', *extra], cwd=ROOT, env=env)
    if script == 'serve':
        common.run(['node', 'scripts/serve.mjs', *extra], cwd=ROOT, env=env)
    elif script in ('start', 'simulate'):
        if script == 'simulate':
            env['CORDIAL_DESKTOP_SIMULATE'] = '2'
        common.run([ROOT / 'node_modules/.bin/electron', '.', *extra], cwd=ROOT, env=env)


if __name__ == '__main__':
    common.main(main)
