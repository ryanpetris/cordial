"""Small Linux bootstrap helpers shared by the two ecosystem entry points."""
from contextlib import contextmanager
import fcntl
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
TOOLS = ROOT / '.tools'
CACHE = ROOT / '.cache' / 'bootstrap'


def run(args, **kwargs):
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def output(args, **kwargs):
    return subprocess.check_output(list(map(str, args)), text=True, **kwargs).strip()


def host():
    if not hasattr(tarfile, 'data_filter'):
        raise ValueError('Manual download required: Python 3.11.4+ with tarfile extraction filters')
    try:
        import lzma
    except ImportError:
        raise ValueError('Manual download required: Python 3.11+ with lzma support') from None
    machine = platform.machine()
    if sys.platform != 'linux' or machine not in ('x86_64', 'aarch64'):
        raise ValueError('Manual download required: build toolchains for ' + platform.platform())
    return machine


def missing(arch, debian):
    try:
        distro = platform.freedesktop_os_release()
    except OSError:
        distro = {}
    ids = (distro.get('ID', '') + ' ' + distro.get('ID_LIKE', '')).split()
    if 'arch' in ids:
        return 'Missing packages: ' + ' '.join(sorted(set(arch)))
    if 'debian' in ids:
        return 'Missing packages: ' + ' '.join(sorted(set(debian)))
    return ('Missing packages (Arch): ' + ' '.join(sorted(set(arch))) +
            '\nMissing packages (Debian): ' + ' '.join(sorted(set(debian))))


def require(commands=(), *, venv=False):
    """Each command has its Arch and Debian package names."""
    arch, debian = [], []
    for command, a, d in commands:
        if not shutil.which(command):
            arch.append(a)
            debian.append(d)
    if venv:
        try:
            import ensurepip  # noqa: F401
        except ImportError:
            arch.append('python')
            debian.append('python3-venv')
    if arch:
        raise ValueError(missing(arch, debian))


NATIVE = [('cc', 'gcc', 'build-essential'), ('c++', 'gcc', 'build-essential'),
          ('ar', 'binutils', 'binutils'), ('git', 'git', 'git')]
CMAKE = [('cmake', 'cmake', 'cmake'), ('ninja', 'ninja', 'ninja-build')]


def prepend(env, *paths):
    env['PATH'] = os.pathsep.join(map(str, paths)) + os.pathsep + env.get('PATH', '')


@contextmanager
def lock(name):
    CACHE.mkdir(parents=True, exist_ok=True)
    with (CACHE / (name + '.lock')).open('a') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print('Waiting for ' + name + ' build lock', flush=True)
            fcntl.flock(handle, fcntl.LOCK_EX)
        yield


def download(url, digest, name):
    CACHE.mkdir(parents=True, exist_ok=True)
    target = CACHE / name
    if target.exists():
        with target.open('rb') as data:
            if hashlib.file_digest(data, 'sha256').hexdigest() == digest:
                return target
    print('Downloading ' + name, flush=True)
    with tempfile.NamedTemporaryFile(dir=CACHE, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            with urllib.request.urlopen(url, timeout=120) as source:
                shutil.copyfileobj(source, stream)
            stream.close()
            with temporary.open('rb') as data:
                if hashlib.file_digest(data, 'sha256').hexdigest() != digest:
                    raise ValueError('Checksum mismatch: ' + name)
            temporary.replace(target)
        finally:
            temporary.unlink(missing_ok=True)
    return target


def unpack(archive, destination):
    """Publish a complete extraction; failed extractions never become tools."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temp:
        with tarfile.open(archive) as bundle:
            bundle.extractall(temp, filter='data')
        roots = list(Path(temp).iterdir())
        if len(roots) != 1 or not roots[0].is_dir():
            raise ValueError('Unexpected tool archive layout: ' + str(archive))
        if destination.exists():
            shutil.rmtree(destination)
        roots[0].rename(destination)


def main(action):
    try:
        host()
        action()
    except (ValueError, OSError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
