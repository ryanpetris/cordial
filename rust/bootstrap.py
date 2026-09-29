#!/usr/bin/env python3
"""Prepare Rust and board toolchains, then run a Rust Make target."""
import argparse
import ctypes
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
import bootstrap as common
sys.path.insert(0, str(Path(__file__).resolve().parent / 'tools'))
import firmware_config
import firmware_dependencies

ROOT = common.ROOT / 'rust'
RUSTUP_HASHES = {
    'x86_64': '20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c',
    'aarch64': 'e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c',
}
ARM_VERSION = '14.3.rel1'
ARM_HASHES = {
    'x86_64': '8f6903f8ceb084d9227b9ef991490413014d991874a1e34074443c2a72b14dbd',
    'aarch64': '2d465847eb1d05f876270494f51034de9ace9abe87a4222d079f3360240184d3',
}
ESPUP_HASHES = {
    'x86_64': 'dbe54e9907b687809dbe1b955731569ed6df2b525362710d676256c5c8cf9ccd',
    'aarch64': '2c2275cc937f33f64d65bed4338aa4216f2c70c9dfe5e563b9942cd936088b0c',
}


def rust(env, targets=(), clippy=False):
    # A rustup proxy must win over distro cargo/rustc, including for +cordial-esp.
    rustup = shutil.which('rustup', path=env['PATH'])
    if not rustup:
        user_rustup = Path(env.get('CARGO_HOME', Path.home() / '.cargo')) / 'bin/rustup'
        if user_rustup.is_file():
            rustup = str(user_rustup)
    if not rustup:
        local = common.TOOLS / 'rust'
        env.update(CARGO_HOME=str(local / 'cargo'), RUSTUP_HOME=str(local / 'rustup'))
        rustup = str(local / 'cargo/bin/rustup')
        if not Path(rustup).is_file():
            arch = common.host()
            installer = common.download(
                f'https://static.rust-lang.org/rustup/archive/1.28.2/{arch}-unknown-linux-gnu/rustup-init',
                RUSTUP_HASHES[arch], f'rustup-init-1.28.2-{arch}')
            installer.chmod(0o755)
            common.run([installer, '-y', '--no-modify-path', '--default-toolchain', 'none'], env=env)
    proxies = common.TOOLS / 'rust-bin'
    proxies.mkdir(parents=True, exist_ok=True)
    for name in ('rustup', 'cargo', 'rustc', 'rustdoc', 'cargo-clippy', 'clippy-driver'):
        proxy = proxies / name
        proxy.unlink(missing_ok=True)
        proxy.symlink_to(Path(rustup).absolute())
    common.prepend(env, proxies)
    toolchain = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    env['RUSTUP_TOOLCHAIN'] = toolchain
    probe = subprocess.run(['rustup', 'which', '--toolchain', toolchain, 'rustc'],
                           env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if probe.returncode:
        common.run(['rustup', 'toolchain', 'install', toolchain, '--profile', 'minimal', '--no-self-update'], env=env)
    installed = common.output(['rustup', 'target', 'list', '--installed'], env=env).splitlines()
    for target in targets:
        if target not in installed:
            common.run(['rustup', 'target', 'add', target], env=env)
    if clippy and not any(line.startswith('clippy-') for line in
                          common.output(['rustup', 'component', 'list', '--installed'], env=env).splitlines()):
        common.run(['rustup', 'component', 'add', 'clippy'], env=env)


def libclang(env):
    roots = [Path('/usr/lib'), Path('/usr/lib') / (common.host() + '-linux-gnu')]
    roots += sorted(Path('/usr/lib').glob('llvm-*/lib'), reverse=True)
    if env.get('LIBCLANG_PATH'):
        roots.insert(0, Path(env['LIBCLANG_PATH']))
    for root in roots:
        candidates = [root] if root.is_file() else sorted(root.glob('libclang*.so*'))
        for candidate in candidates:
            try:
                library = ctypes.CDLL(str(candidate))
                library.clang_createIndex
            except (OSError, AttributeError):
                continue
            env['LIBCLANG_PATH'] = str(candidate.parent)
            return
    raise ValueError(common.missing(['clang'], ['libclang-dev']))


def arm(env, config):
    flags = (['-mcpu=cortex-m0plus', '-mthumb'] if config['chip'] == 'rp2040' else
             ['-mcpu=cortex-m33', '-mthumb', '-mfloat-abi=hard', '-mfpu=fpv5-sp-d16'])
    def usable(compiler):
        if not compiler:
            return False
        result = subprocess.run([compiler, *flags, '-print-file-name=libc_nano.a'],
                                capture_output=True, text=True, env=env)
        if result.returncode or not Path(result.stdout.strip()).is_file():
            return False
        from dependency_notices import newlib_notice
        try:
            newlib_notice(compiler)
        except ValueError:
            return False
        return True
    if usable(shutil.which('arm-none-eabi-gcc', path=env['PATH'])):
        return
    arch = common.host()
    name = f'arm-gnu-toolchain-{ARM_VERSION}-{arch}-arm-none-eabi'
    destination = common.TOOLS / name
    compiler = destination / 'bin/arm-none-eabi-gcc'
    if not compiler.exists() or not usable(compiler):
        archive = common.download(
            f'https://developer.arm.com/-/media/Files/downloads/gnu/{ARM_VERSION}/binrel/{name}.tar.xz',
            ARM_HASHES[arch], name + '.tar.xz')
        common.unpack(archive, destination)
    common.prepend(env, destination / 'bin')


def esp(env):
    arch = common.host()
    espup = common.download(
        f'https://github.com/esp-rs/espup/releases/download/v0.17.1/espup-{arch}-unknown-linux-gnu',
        ESPUP_HASHES[arch], f'espup-0.17.1-{arch}')
    espup.chmod(0o755)
    probe = subprocess.run(['rustc', '+cordial-esp', '--version'], env=env, capture_output=True, text=True)
    complete = False
    if probe.returncode == 0 and '1.97.0.0' in probe.stdout:
        root = Path(common.output(['rustc', '+cordial-esp', '--print', 'sysroot'], env=env))
        complete = (any(root.glob('xtensa-esp32-elf-clang/*/esp-clang/lib/libclang.so*'))
                    and (root / 'lib/rustlib/src/rust/library/std').is_dir())
    if not complete:
        common.run([espup, 'install', '--name', 'cordial-esp', '--toolchain-version', '1.97.0.0',
                    '--targets', 'esp32s3', '--std', '--export-file', common.TOOLS / 'esp-export.sh'], env=env)
    linker = common.TOOLS / 'esp/bin/ldproxy'
    if not linker.is_file():
        common.run(['cargo', 'install', 'ldproxy', '--version', '0.3.5', '--locked',
                    '--root', common.TOOLS / 'esp'], env=env)
    common.prepend(env, linker.parent)
    # Use the SDK's own pinned tool manifest and checksummed installer.
    sdk = firmware_dependencies.prepare_git('esp-idf', 'https://github.com/espressif/esp-idf.git',
                                            'fff9895c82d744c7237be8847347bdd1b07c6643')
    common.run(['git', '-C', sdk, 'config', 'remote.origin.url', 'https://github.com/espressif/esp-idf.git'], env=env)
    if 'v6.1' not in common.output(['git', '-C', sdk, 'tag', '--points-at', 'HEAD'], env=env).splitlines():
        common.run(['git', '-C', sdk, 'fetch', '--depth', '1', 'origin', 'tag', 'v6.1'], env=env)
    if common.output(['git', '-C', sdk, 'rev-parse', 'v6.1^{commit}'], env=env) != 'fff9895c82d744c7237be8847347bdd1b07c6643':
        raise ValueError('ESP-IDF v6.1 tag differs from the pinned commit')
    common.run(['git', '-C', sdk, 'submodule', 'sync', '--recursive'], env=env)
    common.run(['git', '-C', sdk, 'submodule', 'update', '--init', '--recursive', '--depth', '1'], env=env)
    from build_firmware import SDK_TOOLS
    env.update(IDF_PATH=str(sdk), IDF_TOOLS_PATH=str(SDK_TOOLS))
    installer = [sys.executable, sdk / 'tools/idf_tools.py']
    # esp-idf-sys installs its other build tools; flashing/debug tools are not needed.
    common.run(installer + ['install', 'xtensa-esp-elf', '--targets=esp32s3'], env=env)
    check = subprocess.run(installer + ['check-python-dependencies'], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if check.returncode:
        common.run(installer + ['install-python-env'], env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['cli', 'firmware', 'package-cli', 'package-firmware', 'check', 'check-memory', 'schema'])
    parser.add_argument('--board', default='pico_w')
    parser.add_argument('--profile', default='development', choices=['development', 'production'])
    args = parser.parse_args()
    board = ROOT / 'boards' / (args.board + '.json')
    firmware = args.action in ('firmware', 'package-firmware', 'check-memory')
    config = firmware_config.load(board, args.profile) if firmware else None
    is_esp = firmware and config['chip'] == 'esp32s3'
    if args.action == 'check-memory':
        if args.board != 'pico_w' or args.profile != 'development':
            parser.error('check-memory requires Pico W development firmware')
        common.require([('qemu-system-arm', 'qemu-system-arm', 'qemu-system-arm')])
    commands = common.NATIVE + (common.CMAKE if firmware else [])
    if is_esp:
        commands += [('pkg-config', 'pkgconf', 'pkg-config')]
    elif firmware or args.action == 'check':
        commands += [('clang', 'clang', 'clang')]
    common.require(commands, venv=is_esp)
    if firmware:
        version = tuple(map(int, common.output(['cmake', '--version']).splitlines()[0].split()[-1].split('.')[:2]))
        if version < (3, 24):
            raise ValueError('Manual download required: CMake 3.24+ (https://cmake.org/download/)')
    env = dict(os.environ, CORDIAL_PYTHON=sys.executable)
    if (firmware and not is_esp) or args.action == 'check':
        libclang(env)
    targets = [config['target']] if firmware and not is_esp else []
    if args.action == 'package-cli':
        targets.append(common.host() + '-unknown-linux-musl')
    # Hold through the build: the firmware SDK cache is shared across boards.
    with common.lock('rust'):
        rust(env, targets, clippy=args.action == 'check')
        if firmware:
            esp(env) if is_esp else arm(env, config)
            common.run([sys.executable, ROOT / 'tools/build_firmware.py', board, '--profile', args.profile,
                        *(['--release'] if args.action == 'package-firmware' else [])], env=env)
            if args.action == 'check-memory':
                version = common.output([sys.executable, common.ROOT / 'tools/version.py'], env=env)
                name = f"{config['name']}-{config['bluetooth_backend']}-{config['radio_backend']}-{args.profile}"
                elf = common.ROOT / 'build/firmware' / version / name / ('cordial-' + name + '.elf')
                common.run([sys.executable, ROOT / 'tools/check_memory.py', elf], env=env)
        elif args.action == 'package-cli':
            common.run([sys.executable, ROOT / 'tools/build_host.py', '--release'], env=env)
        elif args.action == 'check':
            firmware_dependencies.prepare_btstack()
            for command in (['cargo', 'run', '--locked', '-p', 'cordial-schema', '--', '--check'],
                            ['cargo', 'test', '--locked', '--workspace', '--all-features'],
                            ['cargo', 'clippy', '--locked', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings'],
                            [sys.executable, ROOT / 'tools/test_btstack.py']):
                common.run(command, cwd=ROOT, env=env)
        elif args.action == 'schema':
            common.run(['cargo', 'run', '--locked', '-p', 'cordial-schema'], cwd=ROOT, env=env)
        else:
            common.run(['cargo', 'build', '--locked', '--release', '-p', 'cordial-client', '--bin', 'cordial'], cwd=ROOT, env=env)


if __name__ == '__main__':
    common.main(main)
