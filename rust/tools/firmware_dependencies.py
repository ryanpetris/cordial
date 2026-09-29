"""Fetch pinned, unchanged firmware dependencies into the ignored build cache."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT.parent / ".cache/dependencies"
BTSTACK_REVISION = "e38553977a25fb0b55b383c72c289be0975f422c"
PICO_SDK_REVISION = "079c6f39023649b154152db30f1d781e884879bc"
CYW43_DRIVER_REVISION = "bed438f119cfba03f75957fceb6a5623ef95e6d9"
CONTROLLER_REVISION = "50c6aac5b9b3b0af18f5a7d61725cc8e4d1f7acb"
CONTROLLER_FILES = {
    "43439A0.bin": "5555e0261da2610a500d68c18d895cace0152bbefbf76f4aa683ebce77e3d7eb",
    "43439A0_btfw.bin": "ce1992c1a6a16ae51bc012439486e9fb212623eca92d9e82a8090c2acf7ef1df",
    "nvram_rp2040.bin": "4904bdbb0c937bd0ac2eb2a1d62f2da4dd90e32082384e02874e8d671b0f330d",
    "LICENSE-permissive-binary-license-1.0.txt": "5f65b8a496ac27afda41917c18cb6e690b4a022df1f5a12ea823eb38a287f50e",
}


def prepare_git(name, url, revision):
    source = CACHE / name
    source.mkdir(parents=True, exist_ok=True)

    def git(*args, capture=False):
        result = subprocess.run(["git", "-C", str(source), *args], check=True,
                                capture_output=capture, text=True)
        return result.stdout.strip() if capture else None

    if not (source / ".git").exists():
        git("init", "-q")
    if git("status", "--porcelain", "--untracked-files=all", capture=True):
        raise ValueError(f"Cached {name} must be unmodified")
    head = subprocess.run(["git", "-C", str(source), "rev-parse", "HEAD"],
                          capture_output=True, text=True)
    if head.returncode or head.stdout.strip() != revision:
        git("fetch", "--depth", "1", url, revision)
        git("checkout", "--detach", "FETCH_HEAD")
    return source


def prepare_btstack():
    return prepare_git("btstack", "https://github.com/bluekitchen/btstack.git", BTSTACK_REVISION)


def prepare_sdk():
    sdk = prepare_git("pico-sdk", "https://github.com/raspberrypi/pico-sdk.git", PICO_SDK_REVISION)
    driver = prepare_git("cyw43-driver", "https://github.com/georgerobotics/cyw43-driver.git", CYW43_DRIVER_REVISION)
    return sdk, driver


def prepare():
    stack = prepare_btstack()
    firmware = CACHE / "cyw43-firmware" / CONTROLLER_REVISION
    firmware.mkdir(parents=True, exist_ok=True)
    for name, digest in CONTROLLER_FILES.items():
        target = firmware / name
        if target.exists():
            data = target.read_bytes()
        else:
            url = f"https://raw.githubusercontent.com/embassy-rs/embassy/{CONTROLLER_REVISION}/cyw43-firmware/{name}"
            with urllib.request.urlopen(url, timeout=60) as response:
                data = response.read()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"Controller dependency checksum mismatch: {name}")
        if not target.exists():
            temporary = target.with_suffix(target.suffix + ".tmp")
            temporary.write_bytes(data)
            temporary.replace(target)
    return stack, firmware


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    selection = parser.add_mutually_exclusive_group(required=True)
    selection.add_argument("--radio", choices=("embassy-cyw43", "pico-sdk-cyw43"))
    selection.add_argument("--btstack", action="store_true")
    args = parser.parse_args()
    paths = [prepare_btstack()] if args.btstack else (prepare_sdk() if args.radio == "pico-sdk-cyw43" else prepare())
    for path in paths:
        print(path)
