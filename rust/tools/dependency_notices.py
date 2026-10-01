"""Record the selected Cargo graph and preserve supplied dependency notices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

import firmware_dependencies as deps


def newlib_notice(compiler):
    root = Path(compiler).resolve().parents[1]
    for relative in ('share/licenses/arm-none-eabi-newlib/COPYING.NEWLIB',
                     'share/doc/libnewlib-arm-none-eabi/copyright',
                     'share/doc/libnewlib-dev/copyright', 'license.txt'):
        path = root / relative
        if path.is_file() and 'newlib' in path.read_text(encoding='utf-8', errors='replace').lower():
            return path
    raise ValueError('Missing newlib redistribution notice for ' + str(compiler))


def record(cargo, platform, features, target, env, output, sdk=None):
    feature_flags = ["--features", ",".join(features)]
    if platform != "host":
        feature_flags.append("--no-default-features")
    # Metadata may need sources for workspace members and targets not compiled
    # by the build. Cargo fetches those sources using the committed lockfile.
    metadata = json.loads(subprocess.check_output(
        cargo + ["metadata", "--locked", "--format-version", "1", "--manifest-path",
                 str(deps.ROOT / ("crates/cordial-cli/Cargo.toml" if platform == "host" else f"platforms/{platform}/Cargo.toml")), "--filter-platform", target,
                 *feature_flags], env=env, cwd=deps.ROOT))
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending, selected = [metadata["resolve"]["root"]], set()
    while pending:
        package = pending.pop()
        if package in selected:
            continue
        selected.add(package)
        pending.extend(dep["pkg"] for dep in nodes[package]["deps"]
                       if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]))
    notices = output / "notices"
    notices.mkdir()
    entries = []

    def copy(path, name):
        destination = notices / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
        return {"file": str(destination.relative_to(output)),
                "sha256": hashlib.sha256(destination.read_bytes()).hexdigest()}

    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in selected or package["source"] is None:
            continue
        root = Path(package["manifest_path"]).parent
        prefix = f"crates/{package['name']}-{package['version']}"
        texts = [copy(path, f"{prefix}/{path.name}") for path in sorted(root.iterdir())
                 if path.is_file() and path.name.upper().startswith(
                     ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE"))]
        if package["license_file"]:
            path = root / package["license_file"]
            if not any(Path(item["file"]).name == path.name for item in texts):
                texts.append(copy(path, f"{prefix}/{path.name}"))
        vcs = root / ".cargo_vcs_info.json"
        entries.append({key: package[key] for key in ("name", "version", "license", "authors", "repository", "source")} | {
            "features": nodes[package["id"]]["features"], "notices": texts,
            "vcs": json.loads(vcs.read_text()) if vcs.exists() else None})
    native = []
    compiler = env.get("CC_" + target.replace("-", "_").replace(".", "_"))
    if compiler is not None:
        native.append({"name": "GNU compiler runtime (libgcc)",
                       "license": "GPL-3.0-or-later WITH GCC-exception-3.1",
                       "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
                       "source": "https://gcc.gnu.org/onlinedocs/libstdc++/manual/license.html"})
    if "btstack" in features or platform == "pico":
        stack = deps.CACHE / "btstack"
        native.append({"name": "BTstack", "revision": deps.BTSTACK_REVISION, "notices": [
            copy(stack / "LICENSE", "btstack/LICENSE"),
            copy(stack / "3rd-party/micro-ecc/LICENSE.txt", "btstack/micro-ecc.txt")],
            "rijndael": "Public domain; Philip J. Erdelsky; 3rd-party/rijndael/rijndael.c header"})
    if platform == "pico":
        native[-1]["notices"].append(copy(deps.ROOT.parent / "notices/raspberry-pi-btstack.txt", "btstack/raspberry-pi.txt"))
        if "embassy-cyw43" in features:
            firmware = deps.CACHE / "cyw43-firmware" / deps.CONTROLLER_REVISION
            native.append({"name": "CYW43439 controller assets", "revision": deps.CONTROLLER_REVISION,
                           "files": deps.CONTROLLER_FILES, "notices": [copy(
                               firmware / "LICENSE-permissive-binary-license-1.0.txt", "cyw43/LICENSE.txt")]})
        else:
            pico = deps.CACHE / "pico-sdk"
            driver = deps.CACHE / "cyw43-driver"
            native.append({"name": "Pico SDK radio components", "revision": deps.PICO_SDK_REVISION,
                           "notices": [copy(pico / "LICENSE.TXT", "pico-sdk/LICENSE.TXT")]})
            native.append({"name": "CYW43 C driver", "revision": deps.CYW43_DRIVER_REVISION,
                           "notices": [copy(driver / "LICENSE", "cyw43-driver/LICENSE")]})
            blobs = driver / "firmware"
            native.append({"name": "CYW43439 controller assets", "revision": deps.CYW43_DRIVER_REVISION,
                           "files": {name: hashlib.sha256((blobs / name).read_bytes()).hexdigest()
                                     for name in ("wb43439A0_7_95_49_00_combined.h", "cyw43_btfw_43439.h", "wifi_nvram_43439.h")},
                           "notices": [copy(driver / "LICENSE", "cyw43-driver/LICENSE"),
                                       copy(blobs / "README.md", "cyw43-driver/firmware-README.md"),
                                       copy(deps.ROOT.parent / "notices/cyw43-permissive-binary-license-1.0.txt",
                                            "cyw43-driver/LICENSE-permissive-binary-license-1.0.txt")]})
            license = newlib_notice(compiler)
            native.append({"name": "newlib C string helpers", "source": "https://sourceware.org/newlib/",
                           "notices": [copy(license, "newlib/" + ("arm-gnu-toolchain-license.txt" if license.name == "license.txt" else license.name))]})
    if sdk:
        paths = [sdk / "LICENSE"]
        # Preserve the SDK's supplied notices, including components used only by
        # its tools or optional configurations. This is not a link-map claim.
        paths += [p for p in sorted((sdk / "components").rglob("*")) if p.is_file()
                  and p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE"))]
        native.append({"name": "ESP-IDF", "revision": subprocess.check_output(
            ["git", "-C", str(sdk), "rev-parse", "HEAD"], text=True).strip(),
            "notices": [copy(p, "esp-idf/" + str(p.relative_to(sdk))) for p in paths]})
    rustc = ["rustc"] + ([cargo[1]] if len(cargo) > 1 else [])
    toolchain = subprocess.check_output(rustc + ["--version", "--verbose"],
                                        env=env, cwd=deps.ROOT, text=True).strip()
    root = Path(subprocess.check_output(rustc + ["--print", "sysroot"],
                                        env=env, cwd=deps.ROOT, text=True).strip())
    for file in ("COPYRIGHT-library.html", "COPYRIGHT.html"):
        path = root / "share/doc/rust" / file
        if path.exists():
            copy(path, "rust/" + file)
    for path in (root / "share/doc/rust/licenses").glob("*"):
        if path.is_file():
            copy(path, "rust/licenses/" + path.name)
    (output / "dependencies.json").write_text(json.dumps({
        "target": target, "rustc": toolchain,
        "scope": "Selected Cargo graph, including build dependencies. SDK notices include optional components; entries do not imply linked bytes.",
        "crates": entries, "native": native}, indent=2) + "\n")
    (output / "DEPENDENCIES.md").write_text(
        "# Dependency inventory\n\n"
        "`dependencies.json` records selected crate versions, features, licence declarations, authors and source revisions. "
        "`notices/` preserves licence and notice files supplied by those crate archives and the native SDK. "
        "Some published archives omit their licence texts; their declarations and upstream repositories remain in the inventory. "
        "The SDK notice collection also includes optional components and build tools; use the linker map to determine linked components. "
        "This inventory does not establish Bluetooth qualification or a commercial BTstack licence.\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    record(["cargo"], "host", [], args.target, os.environ, args.output)
