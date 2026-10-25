"""Check copied source freshness and the shared Docker build/export contract."""
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("build_cache", ROOT / "docker/build-cache.py")
build_cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_cache)


class BuildCache(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("CORDIAL_TEST_DOCKER"),
                         "set CORDIAL_TEST_DOCKER to run the tiny npm Docker fixture")
    def test_application_install_layer_reuse_and_failed_retries(self):
        docker = shlex.split(os.environ["CORDIAL_TEST_DOCKER"])

        def run(arguments, **kwargs):
            result = subprocess.run(arguments, capture_output=True, text=True, **kwargs)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return result.stdout

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            for name in ("Makefile", ".dockerignore", "docker/build.Dockerfile", "docker/build-cache.py"):
                destination = source / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / name, destination)
            desktop = source / "desktop"
            workspace = desktop / "packages/client"
            workspace.mkdir(parents=True)
            (workspace / "package.json").write_text('{"name":"@fixture/client","version":"1.0.0"}')
            (desktop / "source.txt").write_text("first")
            install = ("const fs=require('fs');"
                       "fs.mkdirSync('packages/client/node_modules/nested',{recursive:true});"
                       "fs.writeFileSync('packages/client/node_modules/nested/index.js','module.exports=42');"
                       "fs.writeFileSync('node_modules/token',require('crypto').randomUUID());")
            build = ("const fs=require('fs');"
                     "if(!fs.lstatSync('node_modules/@fixture/client').isSymbolicLink())process.exit(1);"
                     "fs.mkdirSync('out/web',{recursive:true});"
                     "fs.writeFileSync('out/web/result.json',JSON.stringify({"
                     "token:fs.readFileSync('node_modules/token','utf8'),"
                     "source:fs.readFileSync('source.txt','utf8'),"
                     "nested:require('./packages/client/node_modules/nested')}));")
            package = {"name": "npm-fixture", "version": "1.0.0", "private": True,
                       "workspaces": ["packages/*"],
                       "devDependencies": {"@fixture/client": "1.0.0"},
                       "scripts": {"postinstall": "node -e " + shlex.quote(install),
                                   "build:web": "node -e " + shlex.quote(build)}}
            package_path = desktop / "package.json"
            package_path.write_text(json.dumps(package))
            (source / ".npmrc").write_text("ignore-scripts=true\n")
            (desktop / ".npmrc").write_text("offline=true\naudit=false\nfund=false\n")
            run(["npm", "install", "--package-lock-only", "--ignore-scripts", "--prefix", str(desktop)])
            inputs = root / "inputs"
            inputs.mkdir()
            fixture_id = "build-cache-npm-test-" + uuid.uuid4().hex
            image = os.environ.get("CORDIAL_TEST_APP_IMAGE", "cordial-app")
            image_id = run(docker + ["image", "inspect", "--format", "{{.Id}}", image]).strip()
            image = fixture_id + ":base"
            run(docker + ["tag", image_id, image])
            self.addCleanup(run, docker + ["image", "rm", image])

            def build_image(name, command="make local-web", failure=False, toolchain=image):
                output = root / name
                result = subprocess.run(docker + ["build", "--progress=plain", "-f", "docker/build.Dockerfile",
                    "--build-arg", "IMAGE=" + toolchain, "--build-arg", "CACHE_ID=" + fixture_id,
                    "--build-arg", "DEPENDENCY_ID=fixture", "--build-arg", "PREPARE_STAGE=application",
                    "--build-arg", "COMMAND=" + command, "--build-arg", "OUTPUTS=desktop/out/web",
                    "--build-context", "inputs=" + str(inputs),
                    "--output", "type=local,dest=" + str(output), "."],
                    cwd=source, capture_output=True, text=True)
                log = result.stdout + result.stderr
                if failure:
                    self.assertNotEqual(result.returncode, 0, log)
                    self.assertIn("fixture-postinstall-failed", log)
                    self.assertFalse(output.exists())
                    return
                self.assertEqual(result.returncode, 0, log)
                self.assertFalse(list(output.rglob("node_modules")))
                record = json.loads((output / "desktop/out/web/result.json").read_text())
                self.assertEqual(record["nested"], 42)
                record["log"] = log
                return record

            first = build_image("first")
            repeated = build_image("repeat", command="make local-web && : repeat")
            self.assertEqual(first["token"], repeated["token"], repeated["log"])
            (desktop / "source.txt").write_text("edited")
            edited = build_image("source-edit")
            self.assertEqual(first["token"], edited["token"])
            self.assertEqual(edited["source"], "edited")
            (source / ".npmrc").write_text("ignore-scripts=false\n")
            root_config = build_image("root-config-edit")
            self.assertEqual(edited["token"], root_config["token"])
            previous = root_config
            for index, path in enumerate((package_path, desktop / "package-lock.json",
                                          workspace / "package.json", desktop / ".npmrc")):
                path.write_text(path.read_text() + "\n")
                changed = build_image(f"input-{index}")
                self.assertNotEqual(previous["token"], changed["token"])
                previous = changed
            runtime = root / "runtime"
            runtime.mkdir()
            (runtime / "Dockerfile").write_text("ARG IMAGE\nFROM ${IMAGE}\nENV NPM_FIXTURE_RUNTIME=changed\n")
            run(docker + ["build", "--build-arg", "IMAGE=" + image, "-t", fixture_id, str(runtime)])
            try:
                changed = build_image("runtime-change", toolchain=fixture_id)
                self.assertNotEqual(previous["token"], changed["token"])
            finally:
                run(docker + ["image", "rm", fixture_id])
            package["scripts"]["postinstall"] += "; echo fixture-postinstall-failed >&2; exit 17"
            package_path.write_text(json.dumps(package))
            build_image("failure", failure=True)
            build_image("failure-retry", failure=True)
            package["scripts"]["postinstall"] = "node -e " + shlex.quote(install)
            package_path.write_text(json.dumps(package))
            self.assertEqual(build_image("recovery")["source"], "edited")

    def test_npm_inputs_follow_all_workspaces_and_install_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "src"
            files = {
                "desktop/package.json": json.dumps({"workspaces": ["packages/*", "other/nested/*"]}),
                "desktop/package-lock.json": "lock",
                "desktop/npm-shrinkwrap.json": "shrinkwrap",
                ".npmrc": "audit=false",
                "desktop/.npmrc": "fund=false",
                "desktop/packages/client/package.json": '{"name":"client"}',
                "desktop/other/nested/protocol/package.json": '{"name":"protocol"}',
                "desktop/packages/client/src/index.ts": "application source",
                "desktop/node_modules/dependency/package.json": "installed dependency",
                "desktop/packages/client/node_modules/nested/package.json": "nested dependency",
            }
            for name, contents in files.items():
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)

            def snapshot(name):
                destination = root / name
                build_cache.npm_inputs(source, destination)
                return {path.relative_to(destination).as_posix(): path.read_bytes()
                        for path in destination.rglob("*") if path.is_file()}

            first = snapshot("first")
            expected = {name for name in files
                        if "node_modules" not in name and "/src/" not in name and name != ".npmrc"}
            self.assertEqual(set(first), expected)
            (source / "desktop/packages/client/src/index.ts").write_text("source edit")
            self.assertEqual(first, snapshot("source-edit"))
            (source / ".npmrc").write_text("ignore-scripts=true")
            self.assertEqual(first, snapshot("root-config-edit"))
            for index, name in enumerate(sorted(expected)):
                path = source / name
                original = path.read_text()
                path.write_text(original + "\n")
                self.assertNotEqual(first, snapshot(f"input-edit-{index}"), name)
                path.write_text(original)
            (source / "desktop/packages/new").mkdir()
            (source / "desktop/packages/new/package.json").write_text('{"name":"new"}')
            self.assertIn("desktop/packages/new/package.json", snapshot("new-workspace"))
            (source / "desktop/.npmrc").unlink()
            self.assertNotIn("desktop/.npmrc", snapshot("removed-config"))

    def test_source_timestamps_follow_contents_across_checkouts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, manifest = root / "src", root / "cache/inputs.json"
            source.mkdir()
            path = source / "main.rs"
            path.write_text("first")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            first = path.stat().st_mtime_ns
            self.assertGreater(first, 1)
            os.utime(path, ns=(2, 2))
            build_cache.prepare(source, manifest)
            self.assertEqual(path.stat().st_mtime_ns, first)
            path.write_text("other")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, first)
            # Returning to earlier contents must invalidate the later compilation too.
            path.write_text("first")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, first)
            unchanged = path.stat().st_mtime_ns
            path.chmod(0o755)
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, unchanged)

    def test_directory_changes_and_cache_mount_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, manifest = root / "src", root / "inputs.json"
            path = source / "rust/src/main.rs"
            path.parent.mkdir(parents=True)
            path.write_text("source")
            for cached in build_cache.CACHES:
                cache = source / cached
                cache.mkdir(parents=True, exist_ok=True)
                (cache / "artifact").write_text("cached")
            for cached in ("desktop/node_modules", "desktop/packages/client/node_modules"):
                cache = source / cached
                cache.mkdir(parents=True)
                (cache / "artifact").write_text("installed")
                os.utime(cache / "artifact", ns=(1, 1))
            workspace_source = source / "desktop/packages/client/src/index.ts"
            workspace_source.parent.mkdir()
            workspace_source.write_text("workspace source")
            link = source / "link"
            link.symlink_to("missing")
            build_cache.prepare(source, manifest)
            first = path.parent.stat().st_mtime_ns
            path.unlink()
            build_cache.prepare(source, manifest)
            self.assertGreater(path.parent.stat().st_mtime_ns, first)
            entries = json.loads(manifest.read_text())
            self.assertIn("link", entries)
            self.assertFalse(any("artifact" in name for name in entries))
            self.assertIn("desktop/packages/client/src/index.ts", entries)
            self.assertEqual((source / "desktop/node_modules/artifact").stat().st_mtime_ns, 1)
            link.unlink()
            link.symlink_to("another")
            build_cache.prepare(source, manifest)
            self.assertNotEqual(entries["link"], json.loads(manifest.read_text())["link"])

    def test_make_prepares_npm_only_for_application_builds(self):
        for target in ("desktop", "web", "package-desktop", "package-desktop-tar", "package-web",
                       "cli", "package-cli", "firmware", "check-firmware",
                       "package-desktop-arch", "package-desktop-deb",
                       "package-cli-arch", "package-cli-deb"):
            with self.subTest(target=target):
                result = subprocess.run(["make", "-n", target, "DESKTOP_ARCHIVE=desktop.tar.gz",
                                         "CLI_ARCHIVE=cli.tar.gz"],
                                        cwd=ROOT, check=True, capture_output=True, text=True)
                application = target in {"desktop", "web", "package-desktop",
                                         "package-desktop-tar", "package-web"}
                self.assertIn("--build-arg PREPARE_STAGE=" +
                              ("application" if application else "toolchain"), result.stdout)

    def test_make_pins_environment_and_exports_only_current_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy2(ROOT / "Makefile", root / "Makefile")
            dependencies = root / "rust/tools/firmware_dependencies.py"
            dependencies.parent.mkdir(parents=True)
            dependencies.write_text("pinned dependency")
            docker = root / "docker-fixture"
            docker.write_text('''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
with open("calls.jsonl", "a") as log:
    log.write(json.dumps(args) + "\\n")
state = Path("images.json")
images = json.loads(state.read_text()) if state.exists() else {}
image = "sha256:" + os.environ["FIXTURE_IMAGE"]
images[image] = {"id": image, "runtime": json.loads(os.environ["FIXTURE_RUNTIME"])}
images["cordial-app"] = images[image]
if args[0] == "tag":
    images[args[2]] = images[args[1]]
state.write_text(json.dumps(images))
if args[:2] == ["image", "inspect"]:
    if args[-1] not in images:
        raise SystemExit(1)
    inspected = images[args[-1]]
    template = args[args.index("--format") + 1]
    if template == "{{.Id}}":
        print(inspected["id"])
    else:
        assert template == '{{json .RootFS}} {{json .Config}} {{json .Os}} {{json .Architecture}} {{json (or .Variant "")}}'
        runtime = inspected["runtime"]
        print(" ".join(json.dumps(runtime.get(field, ""), sort_keys=True) for field in
                       ("RootFS", "Config", "Os", "Architecture", "Variant")))
elif "--output" in args:
    reference = next(arg.split("=", 1)[1] for arg in args if arg.startswith("IMAGE="))
    assert images[reference]["runtime"] == json.loads(os.environ["FIXTURE_RUNTIME"])
    if os.environ.get("FIXTURE_FAIL"):
        raise SystemExit(1)
    destination = Path(args[args.index("--output") + 1].split("dest=", 1)[1])
    output = destination / "build/release"
    output.mkdir(parents=True)
    (output / "cordial-cli-current.tar.gz").write_text("current")
''')
            docker.chmod(0o755)

            runtime = dict(RootFS={"Layers": ["fixture"]}, Config={"Env": ["PATH=/bin"]},
                           Os="linux", Architecture="amd64")

            def run(image, contents=runtime):
                subprocess.run(["make", "package-cli", f"DOCKER={docker}"], cwd=root,
                               env=dict(os.environ, FIXTURE_IMAGE=image,
                                        FIXTURE_RUNTIME=json.dumps(contents)), check=True,
                               capture_output=True, text=True)
                calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
                return next(call for call in reversed(calls) if "--output" in call)

            first = run("a" * 64)
            scratch = root / ".cache/docker/package-cli/out/build/release"
            scratch.mkdir(parents=True, exist_ok=True)
            (scratch / "cordial-cli-stale.tar.gz").write_text("stale")
            same = run("a" * 64)
            self.assertEqual(first, same)
            self.assertFalse((root / "build/release/cordial-cli-stale.tar.gz").exists())
            second = run("b" * 64)
            key = lambda call: next(arg for arg in call if arg.startswith("CACHE_ID="))
            self.assertEqual(key(first), key(second))
            self.assertEqual(first, second)
            reference = "cordial-toolchain:" + key(first).split("=", 1)[1]
            self.assertIn("IMAGE=" + reference, first)
            state = root / "images.json"
            self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "a" * 64)
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
            self.assertEqual([call for call in calls if call[0] == "tag"],
                             [["tag", "sha256:" + "a" * 64, reference]])
            for index, (field, value) in enumerate((
                ("RootFS", {"Layers": ["different toolchain"]}),
                ("Config", {"Env": ["PATH=/usr/bin"]}),
                ("Os", "windows"),
                ("Architecture", "arm64"),
                ("Variant", "v8"),
            )):
                with self.subTest(field=field):
                    changed = run(str(index) * 64, dict(runtime, **{field: value}))
                    self.assertNotEqual(key(second), key(changed))
                    self.assertIn("IMAGE=cordial-toolchain:" + key(changed).split("=", 1)[1], changed)
            images = json.loads(state.read_text())
            images[reference] = images["sha256:" + "0" * 64]
            state.write_text(json.dumps(images))
            repaired = run("b" * 64)
            self.assertEqual(second, repaired)
            self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "b" * 64)
            dependencies.write_text("different pinned dependency")
            third = run("b" * 64)
            key = lambda call: next(arg for arg in call if arg.startswith("DEPENDENCY_ID="))
            self.assertNotEqual(key(second), key(third))
            exported = root / "build/release/cordial-cli-current.tar.gz"
            self.assertEqual(exported.read_text(), "current")
            failed = subprocess.run(["make", "package-cli", f"DOCKER={docker}"], cwd=root,
                                    env=dict(os.environ, FIXTURE_IMAGE="b" * 64,
                                             FIXTURE_RUNTIME=json.dumps(runtime), FIXTURE_FAIL="1"),
                                    capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual(exported.read_text(), "current")


if __name__ == "__main__":
    unittest.main()
