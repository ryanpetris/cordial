"""Run adapter callback regressions against the selected native BTstack build."""
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    build = subprocess.run(
        ["cargo", "build", "-p", "cordial-btstack", "--features", "ffi,classic", "--message-format=json"],
        cwd=ROOT, check=True, capture_output=True, text=True)
    messages = [json.loads(line) for line in build.stdout.splitlines()]
    output = next(Path(msg["out_dir"]) for msg in messages
                  if msg["reason"] == "build-script-executed" and "cordial-btstack" in msg["package_id"])
    source = Path(os.environ.get("CORDIAL_BTSTACK_SOURCE", ROOT.parent / ".cache/dependencies/btstack"))
    for name in ("profiles", "gatt", "information", "layout"):
        executable = output / f"{name}-callback-tests"
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-DENABLE_BLE", "-DENABLE_CLASSIC",
                        "-I", str(ROOT / "adapters/btstack/c"), "-I", str(source / "src"), "-I", str(output),
                        str(ROOT / f"adapters/btstack/tests/{name}.c"), str(output / "libcordial_btstack.a"),
                        "-o", str(executable)], check=True, cwd=ROOT)
        subprocess.run([str(executable)], check=True, cwd=ROOT)
    security = output / "saved-security-tests"
    subprocess.run(["cc", "-std=c11", "-DENABLE_BLE=1", "-ffunction-sections", "-fdata-sections",
                    "-Wl,--gc-sections", "-I", str(ROOT / "adapters/btstack/c"), "-I", str(source / "src"),
                    "-I", str(source / "platform/embedded"), str(ROOT / "adapters/btstack/tests/security.c"),
                    *[str(source / f"src/{name}.c") for name in ("btstack_linked_list", "hci_event", "btstack_util")],
                    "-o", str(security)], check=True, cwd=ROOT)
    subprocess.run([str(security)], check=True, cwd=ROOT)


if __name__ == "__main__":
    main()
