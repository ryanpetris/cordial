"""Check GTK test discovery, subprocess isolation, and failure reporting without a display."""

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, call, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import test_gtk as runner


class GtkRunner(unittest.TestCase):
    def test_discovery_runs_each_desktop_test_separately_and_keeps_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / "tests"
            log = root / "calls.jsonl"
            executable.write_text("""#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if '--list' in sys.argv:
    assert sys.argv[1:] == ['--list', '--ignored', '--format', 'terse']
    print('desktop::first: test')
    print('ui::tui::tests::dump_screens: test')
    print('desktop::failure: test')
    print('desktop::last: test')
    print('desktop::benchmark: benchmark')
else:
    assert sys.argv[2:] == ['--ignored', '--exact', '--nocapture']
    assert 'AT_SPI_BUS_ADDRESS' not in os.environ
    config = Path(os.environ['XDG_CONFIG_HOME'])
    assert not (config / 'saved').exists()
    (config / 'saved').touch()
    with Path(os.environ['GTK_RUNNER_LOG']).open('a') as log:
        log.write(json.dumps([sys.argv[1], os.getpid(), str(config),
            os.environ['GSETTINGS_BACKEND'], os.environ['TMPDIR']]) + '\\n')
    sys.exit(7 if sys.argv[1] == 'desktop::failure' else 0)
""")
            executable.chmod(0o755)
            output = io.StringIO()
            with patch.dict(os.environ, GTK_RUNNER_LOG=str(log), AT_SPI_BUS_ADDRESS="inherited"), \
                    redirect_stdout(output):
                self.assertEqual(runner.run_tests(str(executable)), 1)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual([call[0] for call in calls],
                             ["desktop::first", "desktop::failure", "desktop::last"])
            self.assertEqual(len({call[1] for call in calls}), 3)
            self.assertEqual(len({call[2] for call in calls}), 3)
            for _, _, config, backend, temporary in calls:
                self.assertEqual(backend, "keyfile")
                self.assertFalse(Path(config).exists())
                self.assertFalse(Path(temporary).exists())
            self.assertIn("FAIL desktop::failure", output.getvalue())
            self.assertIn("2 passed, 1 failed", output.getvalue())
            executable.write_text("#!/bin/sh\nprintf 'ui::tui::tests::dump_screens: test\\n'\n")
            with self.assertRaisesRegex(RuntimeError, "No ignored desktop GTK tests"):
                runner.run_tests(str(executable))

    def test_build_selects_the_library_executable_from_cargo_output(self):
        artifact = {"reason": "compiler-artifact", "target": {"name": "cordial", "kind": ["lib"]},
                    "profile": {"test": True}, "executable": "/build/current-library-tests"}
        binary = dict(artifact, target={"name": "cordial", "kind": ["bin"]},
                      executable="/build/binary-tests")
        result = subprocess.CompletedProcess([], 0, "\n".join(map(json.dumps, [binary, artifact])))
        with patch.object(runner.subprocess, "run", return_value=result) as build:
            self.assertEqual(runner.build_test_binary(), artifact["executable"])
            build.assert_called_once()
            args = build.call_args.args[0]
            for option in ("--locked", "--all-features", "--lib", "--no-run", "--message-format=json"):
                self.assertIn(option, args)
        with patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "")):
            with self.assertRaisesRegex(RuntimeError, "Expected one Cordial library"):
                runner.build_test_binary()
        with patch.object(runner.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "")):
            with self.assertRaises(subprocess.CalledProcessError):
                runner.build_test_binary()

    def test_missing_prerequisites_fail_before_building(self):
        with patch.object(sys, "argv", ["test_gtk.py"]), \
                patch.object(runner.shutil, "which", return_value=None), \
                patch.object(runner, "build_test_binary") as build:
            with self.assertRaisesRegex(RuntimeError, "Missing GTK test prerequisites: cargo, xvfb-run"):
                runner.main()
            build.assert_not_called()

    def test_session_cleanup_waits_for_children_after_the_leader_exits(self):
        process = Mock(pid=123, poll=Mock(return_value=0))
        with patch.object(runner.os, "killpg", side_effect=[
                None, None, ProcessLookupError, ProcessLookupError]) as kill, \
                patch.object(runner.time, "monotonic", side_effect=[0, 0, 0.05]), \
                patch.object(runner.time, "sleep") as sleep:
            runner.stop_session(process)
        sleep.assert_called_once_with(0.05)
        self.assertEqual(kill.call_args_list[:3], [
            call(123, signal.SIGTERM), call(123, 0), call(123, 0)])
        with patch.object(runner.os, "killpg") as kill, \
                patch.object(runner.time, "monotonic", side_effect=[0, 5]):
            runner.stop_session(process)
        self.assertEqual(kill.call_args_list, [
            call(123, signal.SIGTERM), call(123, signal.SIGKILL)])


if __name__ == "__main__":
    unittest.main()
