#!/usr/bin/env python3
"""Exercise 32-bit allocations in the heap span of a linked Pico development ELF.

The workloads run with the ELF's board configuration: its heap span and its profile memory
budget, as the firmware build generates it.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess

from firmware_artifact import inspect
from firmware_config import load, profile_memory_budget

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("elf", type=Path)
    args = parser.parse_args()
    metadata = inspect(args.elf.read_bytes())
    if metadata["chip"] not in ("rp2040", "rp2350") or metadata["profile"] != "development":
        parser.error("This allocation comparison requires a Pico development ELF")
    board = metadata["hardware"]
    config = load(ROOT / "boards" / f"{board}.json", "development")
    if config["name"] != board or config["chip"] != metadata["chip"]:
        parser.error(f"The ELF does not match rust/boards/{board}.json")

    def arm_tool(name):
        binary = "arm-none-eabi-" + name
        found = shutil.which(binary)
        if not found:
            raise ValueError(f"Missing {binary}")
        return found

    symbols = {}
    for line in subprocess.check_output([arm_tool("nm"), str(args.elf)], text=True).splitlines():
        columns = line.split()
        if len(columns) == 3:
            symbols[columns[2]] = int(columns[0], 16)
    heap = symbols["_heap_end"] - symbols["__sheap"]
    stack = symbols["_stack_start"] - symbols["_stack_end"]
    static = symbols["__ebss"] - symbols["__sdata"]
    budget = profile_memory_budget(config)
    print(f"{board} ELF: static={static} heap={heap} stack={stack} bytes, "
          f"profile budget {budget}", flush=True)
    if heap <= 0 or stack < 65536:
        parser.error("Invalid heap span or reduced Pico stack reservation")
    tmp = ROOT.parent / "target/tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    target = ROOT.parent / "target/memory-arm"
    # The workloads run on Cortex-M0 under QEMU. Allocation sizes and alignment are the same on
    # the RP2350's Cortex-M33: both are 32-bit targets with 8-byte u64 alignment.
    env = dict(os.environ, TMPDIR=str(tmp), CARGO_TARGET_DIR=str(target),
               CORDIAL_TEST_BOARD=board, CORDIAL_TEST_HEAP_BYTES=str(heap),
               CORDIAL_TEST_PROFILE_MEMORY_BUDGET=budget,
               CC_thumbv6m_none_eabi=arm_tool("gcc"), AR_thumbv6m_none_eabi=arm_tool("ar"))
    subprocess.run(["cargo", "build", "--locked", "--manifest-path", "tests/memory-arm/Cargo.toml",
                    "--target", "thumbv6m-none-eabi", "--release"], cwd=ROOT, env=env, check=True)
    subprocess.run(["qemu-system-arm", "-M", "mps2-an385", "-nographic",
                    "-semihosting-config", "enable=on,target=native", "-kernel",
                    str(target / "thumbv6m-none-eabi/release/cordial-memory-arm")],
                   cwd=ROOT, check=True, timeout=120)


if __name__ == "__main__":
    main()
