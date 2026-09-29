"""Build the selected unmodified SDK radio components as a Rust-linked archive."""
import os
from pathlib import Path
import subprocess
import sys

from firmware_config import load


def build(config_path, out, sdk, driver):
    config = load(config_path, "development")  # Profile does not affect radio code.
    out = Path(out).resolve()
    root = Path(__file__).resolve().parents[1]
    triple = config["target"].replace("-", "_").replace(".", "_")
    gcc = Path(os.environ[f"CC_{triple}"]).resolve()
    subprocess.run([
        "cmake", "--fresh", "-S", str(root / "platforms/pico/c"), "-B", str(out / "radio"), "-G", "Ninja",
        f"-DPICO_SDK_PATH={sdk}", f"-DPICO_CYW43_DRIVER_PATH={driver}",
        "-DPICO_BOARD=cordial", f"-DPICO_BOARD_HEADER_DIRS={out}",
        f"-DPICO_PLATFORM={'rp2040' if config['chip'] == 'rp2040' else 'rp2350-arm-s'}",
        f"-DPICO_HARD_FLOAT_ABI={int(config['chip'] == 'rp2350')}",
        f"-DPICO_TOOLCHAIN_PATH={gcc.parent}", "-DCMAKE_BUILD_TYPE=Release",
        f"-DCORDIAL_CONFIG_DIR={out}",
    ], check=True)
    subprocess.run(["cmake", "--build", str(out / "radio"), "--target", "cordial_radio", "-j", "4"], check=True)
    flags = (["-mcpu=cortex-m0plus", "-mthumb"] if config["chip"] == "rp2040" else
             ["-mcpu=cortex-m33", "-mthumb", "-mfloat-abi=hard", "-mfpu=fpv5-sp-d16"])
    libc = Path(subprocess.check_output([gcc, *flags, "-print-file-name=libc_nano.a"], text=True).strip())
    if not libc.is_file():
        raise ValueError("ARM toolchain's nano C library is missing")
    # Pull only the driver's unresolved C string helpers from newlib.
    print(f"cargo:rustc-link-search=native={libc.parent}")


if __name__ == "__main__":
    build(*sys.argv[1:])
