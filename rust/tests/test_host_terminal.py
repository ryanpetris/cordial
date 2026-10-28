"""Offline executable/PTY checks using missing ports or simulated serial adapters."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import tempfile
import time
import unittest

BINARY = os.environ.get("CORDIAL_TEST_BINARY")


@unittest.skipUnless(sys.platform == "linux" and BINARY and importlib.util.find_spec("pyte"),
                     "set CORDIAL_TEST_BINARY and install pyte for Linux terminal checks")
class Terminal(unittest.TestCase):
    def start(self, tui, json_output=False, trailing=False, adapter=None, blocked=False):
        import fcntl
        import pty
        import termios
        import pyte
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        self.screen = pyte.Screen(100, 38)
        self.stream = pyte.ByteStream(self.screen)
        self.transcript = bytearray()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 100, 0, 0))
        # This path is deliberately not a TTY or a device, so the TUI finds no adapter and
        # the shell's open fails.
        port = str(Path(__file__).resolve().parent / "not-a-serial-device")
        assert not Path(port).exists()
        args = [BINARY, "--port", adapter.port if adapter else port]
        args += (["--json"] if json_output and not trailing else [])
        args += (["tui"] if tui else [])
        args += (["--json"] if json_output and trailing else [])
        stderr = self.slave
        if json_output:
            self.json_output = tempfile.TemporaryFile()
            self.addCleanup(self.json_output.close)
            stderr = self.json_output
        if blocked:
            self.json_reader, stderr = os.pipe()
            self.addCleanup(os.close, self.json_reader)
            self.addCleanup(os.close, stderr)
            self.json_writer = stderr
        self.process = subprocess.Popen(args, stdin=self.slave, stdout=self.slave, stderr=stderr,
                                        env=dict(adapter.env if adapter else os.environ,
                                                 TERM="xterm-256color"))
        self.addCleanup(self.cleanup)

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=3)
        os.close(self.master)
        os.close(self.slave)

    def read(self, duration=0.05):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            ready, _, _ = select.select([self.master], [], [], max(0, min(.02, end-time.monotonic())))
            if ready:
                data = os.read(self.master, 65536)
                self.transcript.extend(data)
                self.stream.feed(data)
                if b"\x1b[6n" in data:
                    os.write(self.master, b"\x1b[1;1R")

    def wait_text(self, text):
        end = time.monotonic() + 4
        while time.monotonic() < end:
            self.read()
            if any(text in row for row in self.screen.display):
                return
        self.fail(f"Missing {text!r}:\n" + "\n".join(self.screen.display))

    def exited(self, expected):
        import termios
        end = time.monotonic() + 3
        while self.process.poll() is None and time.monotonic() < end:
            self.read()
        self.assertEqual(self.process.poll(), expected, bytes(self.transcript[-1000:]))
        self.read()
        self.assertEqual(termios.tcgetattr(self.slave), self.original)

    def adapter(self):
        from terminal_adapter import Adapter
        adapter = Adapter()
        self.addCleanup(adapter.close)
        return adapter

    def records(self):
        self.json_output.seek(0)
        output = self.json_output.read()
        self.assertNotIn(b"\x1b", output)
        return [json.loads(line) for line in output.splitlines()]

    def assert_records(self, adapter, records):
        from google.protobuf.json_format import MessageToDict
        self.assertEqual(records, [MessageToDict(message, preserving_proto_field_name=True)
                                   for message in adapter.sent])

    def completed_status(self, adapter):
        status = adapter.p.Status()
        status.CopyFrom(adapter.status)
        error = status.errors.add(code=adapter.p.ERROR_CODE_STORAGE_FAILED,
                                  reason=adapter.p.ERROR_REASON_STORAGE,
                                  outcome=adapter.p.ERROR_OUTCOME_COMPLETE)
        error.related.add(code=adapter.p.ERROR_CODE_UNSUPPORTED,
                          outcome=adapter.p.ERROR_OUTCOME_NO_EFFECT)
        return status

    def test_completed_diagnostics_wait_until_tui_and_shell_release_the_terminal(self):
        for tui in (False, True):
            with self.subTest(tui=tui):
                adapter = self.adapter()
                adapter.status.CopyFrom(self.completed_status(adapter))
                probe = subprocess.run([BINARY, "--port", adapter.port, "adapter", "status"],
                                       env=adapter.env, capture_output=True, timeout=5)
                self.assertEqual(probe.returncode, 0, probe.stderr)
                logged = b"Completed adapter operation" in probe.stderr
                self.start(tui, adapter=adapter)
                self.wait_text("Terminal Adapter" if tui else "Connected to")
                self.assertNotIn(b"Completed adapter operation", self.transcript)
                os.write(self.master, b"\x03")
                self.exited(0)
                self.assertEqual(b"Completed adapter operation" in self.transcript, logged)
                if logged:
                    self.assertIn(b"related: [Error", self.transcript)
                    if tui:
                        self.assertLess(self.transcript.index(b"\x1b[?1049l"),
                                        self.transcript.index(b"Completed adapter operation"))
                self.doCleanups()

    def test_completed_cli_diagnostics_have_bounded_shutdown_with_a_full_pipe(self):
        adapter = self.adapter()
        adapter.status.CopyFrom(self.completed_status(adapter))
        args = [BINARY, "--port", adapter.port, "adapter", "status"]
        probe = subprocess.run(args, env=adapter.env, capture_output=True, timeout=5)
        self.assertEqual(probe.returncode, 0, probe.stderr)
        for _ in range(4096):
            adapter.status.errors[0].related.add(code=adapter.p.ERROR_CODE_UNSUPPORTED,
                                                outcome=adapter.p.ERROR_OUTCOME_NO_EFFECT)
        process = subprocess.Popen(args, env=adapter.env, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.PIPE)
        try:
            # The child must finish without a reader draining its stderr pipe.
            self.assertEqual(process.wait(timeout=5), 0)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            process.stderr.close()

    def test_json_cli_succeeds_when_stderr_is_a_broken_pipe(self):
        adapter = self.adapter()
        reader, writer = os.pipe()
        os.close(reader)
        try:
            result = subprocess.run(
                [BINARY, "--port", adapter.port, "--json", "adapter", "status"],
                env=adapter.env, stdout=subprocess.PIPE, stderr=writer, timeout=5)
            self.assertEqual(result.returncode, 0)
            self.assertIn(b"Terminal Adapter", result.stdout)
        finally:
            os.close(writer)

    def test_json_completed_errors_retain_the_original_wire_records(self):
        adapter = self.adapter()
        adapter.status.CopyFrom(self.completed_status(adapter))
        result = subprocess.run([BINARY, "--port", adapter.port, "--json", "adapter", "status"],
                                env=adapter.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in result.stderr.splitlines()]
        self.assert_records(adapter, records)
        status = next(record["response"]["status"] for record in records
                      if "status" in record.get("response", {}))
        errors = status["errors"]
        self.assertEqual(errors[0]["outcome"], "ERROR_OUTCOME_COMPLETE")
        self.assertEqual(errors[0]["related"][0]["code"], "ERROR_CODE_UNSUPPORTED")
        self.assertNotIn(b"Completed adapter operation", result.stdout)

    def test_json_help_and_internal_migration_arguments(self):
        help = subprocess.run([BINARY, "--json", "--help"], capture_output=True, timeout=3)
        self.assertEqual(help.returncode, 0)
        self.assertEqual(help.stderr, b"")
        self.assertIn(b"protobuf JSON to stderr", help.stdout)
        self.assertIn(b"[--json] tui", help.stdout)
        ipc = subprocess.run([BINARY, "--json", "migrate", "--ipc"],
                             input=b"", capture_output=True, timeout=3)
        self.assertNotEqual(ipc.returncode, 0)
        self.assertEqual(ipc.stdout, b"")
        self.assertIn(b"migrate --ipc takes no other options", ipc.stderr)

    def test_json_tui_keeps_stdout_interactive_with_stderr_redirected(self):
        for trailing in (False, True):
            with self.subTest(trailing=trailing):
                adapter = self.adapter()
                self.start(True, json_output=True, trailing=trailing, adapter=adapter)
                self.wait_text("Terminal Adapter")
                os.write(self.master, b"?")
                self.wait_text("Help")
                os.write(self.master, b"\x03")
                self.exited(0)
                records = self.records()
                self.assertTrue(any("status" in r.get("response", {}) for r in records))
                self.assertTrue(any("devices" in r.get("response", {}) for r in records))
                self.assertTrue(any("adapter" in r.get("event", {}) for r in records))
                self.assert_records(adapter, records)
                self.assertNotIn(b'{"response":', self.transcript)
                self.assertNotIn(b'{"event":', self.transcript)
                self.assertIn(b"\x1b[?1049l", self.transcript)
                self.doCleanups()

    def test_json_tui_ignores_a_stalled_reader_without_hanging_shutdown(self):
        adapter = self.adapter()
        self.start(True, json_output=True, adapter=adapter, blocked=True)
        self.wait_text("Terminal Adapter")
        adapter.events.put(adapter.p.Message(event=adapter.p.Event(
            scan_found=adapter.p.Candidate(id=9, name="x" * 65536))))
        deadline = time.monotonic() + 3
        while select.select([], [self.json_writer], [], 0)[1] and time.monotonic() < deadline:
            self.read()
        self.assertFalse(select.select([], [self.json_writer], [], 0)[1])
        os.write(self.master, b"\x03")
        self.exited(0)
        self.assertIn(b"\x1b[?1049l", self.transcript)
        self.assertNotIn(b'{"event":', self.transcript)

    def test_json_shell_keeps_prompt_and_complete_stream(self):
        adapter = self.adapter()
        self.start(False, json_output=True, adapter=adapter)
        self.wait_text("Connected to")
        self.wait_text("cordial>")
        os.write(self.master, b"adapter status\r")
        self.wait_text("Terminal Adapter")
        os.write(self.master, b"help\r")
        self.wait_text("Device settings are saved only when you set them")
        os.write(self.master, b"quit\r")
        self.exited(0)
        records = self.records()
        self.assertTrue(any("status" in r.get("response", {}) for r in records))
        self.assertTrue(any("adapter" in r.get("event", {}) for r in records))
        self.assert_records(adapter, records)
        self.assertNotIn(b'{"response":', self.transcript)
        self.assertNotIn(b'{"event":', self.transcript)
        self.assertNotIn(b"\x1b[?1049h", self.transcript)

    def test_json_cli_keeps_human_output_and_complete_file_bytes(self):
        adapter = self.adapter()
        with tempfile.TemporaryDirectory(prefix="cordial-download-") as directory:
            local = Path(directory) / "data.bin"
            result = subprocess.run([BINARY, "--port", adapter.port, "--json", "file", "get",
                                     "/data.bin", str(local)], env=adapter.env,
                                    capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(b"1537 bytes", result.stdout)
            self.assertNotIn(b'{"response":', result.stdout)
            records = [json.loads(line) for line in result.stderr.splitlines()]
            files = [r["response"]["file"] for r in records if "file" in r.get("response", {})]
            self.assertEqual(len(files), 1)
            self.assertEqual(base64.b64decode(files[0]["data"]), adapter.data)
            self.assertEqual(local.read_bytes(), adapter.data)
            self.assert_records(adapter, records)

    def test_json_cli_ignores_a_stalled_reader_after_saving_the_file(self):
        adapter = self.adapter()
        adapter.data = bytes(range(256)) * 512
        with tempfile.TemporaryDirectory(prefix="cordial-download-") as directory:
            local = Path(directory) / "data.bin"
            process = subprocess.Popen([BINARY, "--port", adapter.port, "--json", "file", "get",
                                        "/data.bin", str(local)], env=adapter.env,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                self.assertEqual(process.wait(timeout=5), 0)
                self.assertEqual(local.read_bytes(), adapter.data)
                self.assertIn(b"131072 bytes", process.stdout.read())
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
                process.stdout.close()
                process.stderr.close()

    def test_json_script_keeps_human_results(self):
        adapter = self.adapter()
        result = subprocess.run([BINARY, "--port", adapter.port, "--json"], env=adapter.env,
                                input=b"adapter status\nquit\n", capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"Terminal Adapter", result.stdout)
        self.assertNotIn(b'{"response":', result.stdout)
        records = [json.loads(line) for line in result.stderr.splitlines()]
        self.assertTrue(any("status" in r.get("response", {}) for r in records))
        self.assert_records(adapter, records)

    def test_json_status_receives_the_event_before_command_completion(self):
        from google.protobuf.json_format import MessageToDict
        adapter = self.adapter()
        result = subprocess.run(
            [BINARY, "--port", adapter.port, "--json", "adapter", "status"],
            env=adapter.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        records = [json.loads(line) for line in result.stderr.splitlines()]
        status = MessageToDict(adapter.status, preserving_proto_field_name=True)
        self.assertEqual(records, [{"event": {"adapter": status}},
                                   {"response": {"status": status}},
                                   {"response": {"devices": {}}},
                                   {"event": {"adapter": status}},
                                   {"response": {"status": status}}])
        self.assert_records(adapter, records)

    def test_json_readiness_diagnostic_with_a_stalled_stderr_reader_exits(self):
        adapter = self.adapter()
        adapter.status.ready = False
        adapter.status.name = "x" * 65536
        reader, writer = os.pipe()
        try:
            process = subprocess.Popen(
                [BINARY, "--port", adapter.port, "--json", "--timeout", "1", "device", "list"],
                env=adapter.env, stdout=subprocess.DEVNULL, stderr=writer)
            try:
                deadline = time.monotonic() + 3
                while select.select([], [writer], [], 0)[1] and time.monotonic() < deadline:
                    if process.poll() is not None:
                        self.fail("the process exited before filling stderr")
                    time.sleep(.01)
                self.assertFalse(select.select([], [writer], [], 0)[1])
                # Readiness diagnostics run before the command's timeout. stderr stays unread
                # until the foreground loop and the logger's bounded drain have both ended.
                self.assertEqual(process.wait(timeout=5), 1)
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=3)
        finally:
            os.close(writer)
            os.close(reader)

    def test_json_readiness_and_final_error_preserve_records_and_diagnostics(self):
        adapter = self.adapter()
        adapter.status.ready = False
        result = subprocess.run(
            [BINARY, "--port", adapter.port, "--json", "--timeout", "0.2", "device", "list"],
            env=adapter.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        lines = result.stderr.splitlines()
        records = [json.loads(line) for line in lines if line.startswith(b"{")]
        diagnostics = [line for line in lines if not line.startswith(b"{")]
        self.assert_records(adapter, records)
        self.assertEqual(diagnostics, ["Waiting for adapter readiness\u2026".encode(),
                                       b"operation timed out"])
        self.assertIn(b"Terminal Adapter", result.stdout)
        self.assertNotIn(b'{"', result.stdout)

    def test_json_script_reports_a_malformed_command_after_protocol_records(self):
        adapter = self.adapter()
        result = subprocess.run([BINARY, "--port", adapter.port, "--json"], env=adapter.env,
                                input=b"adapter status\nmalformed\n", capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"Terminal Adapter", result.stdout)
        lines = result.stderr.splitlines()
        self.assertIn(b"unknown command", lines[-1])
        self.assert_records(adapter, [json.loads(line) for line in lines[:-1]])

    def test_tui_ctrl_c_restores_terminal_after_failed_open(self):
        self.start(True)
        self.wait_text("No Adapter Found")
        os.write(self.master, b"\x03")
        self.exited(0)
        self.assertIn(b"\x1b[?1003h", self.transcript)
        self.assertIn(b"\x1b[?1049l", self.transcript)

    def test_tui_external_signals_restore_terminal(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig):
                self.start(True)
                self.wait_text("No Adapter Found")
                self.process.send_signal(sig)
                self.exited(0)
                self.doCleanups()

    def test_tui_help_and_resize(self):
        import fcntl
        import termios
        self.start(True)
        self.wait_text("No Adapter Found")
        os.write(self.master, b"?")
        self.wait_text("Help")
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 10, 40, 0, 0))
        self.process.send_signal(signal.SIGWINCH)
        self.read(.1)
        os.write(self.master, b"\x03")
        self.exited(0)

    def test_shell_help_scrollback_ctrl_c_and_sigint(self):
        self.start(False)
        self.wait_text("Open failed:")
        os.write(self.master, b"help\r")
        end = time.monotonic() + 3
        while b"Device settings are saved only when you set them" not in self.transcript and time.monotonic() < end:
            self.read()
        self.assertIn(b"Device settings are saved only when you set them", self.transcript)
        self.assertIn(b"device set DEV gamepad-output disabled | hid | xinput", self.transcript)
        os.write(self.master, b"unfinished\x03")
        self.exited(0)
        self.doCleanups()
        self.start(False)
        self.wait_text("Open failed:")
        self.process.send_signal(signal.SIGINT)
        self.exited(0)


if __name__ == "__main__":
    unittest.main()
