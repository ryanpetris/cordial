"""Offline executable/PTY checks. Explicit nonexistent port; never opens hardware."""
import importlib.util
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import time
import unittest

BINARY = os.environ.get("CORDIAL_TEST_BINARY")


@unittest.skipUnless(sys.platform == "linux" and BINARY and importlib.util.find_spec("pyte"),
                     "set CORDIAL_TEST_BINARY and install pyte for Linux terminal checks")
class Terminal(unittest.TestCase):
    def start(self, tui):
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
        # This path is deliberately not a TTY or a device. Enumerating the chooser
        # may read OS metadata, but automatic reconnect is disabled after failure.
        port = str(Path(__file__).resolve().parent / "not-a-serial-device")
        assert not Path(port).exists()
        self.process = subprocess.Popen([BINARY, "--port", port] + (["tui"] if tui else []),
                                        stdin=self.slave, stdout=self.slave, stderr=self.slave,
                                        env=dict(os.environ, TERM="xterm-256color"))
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

    def test_tui_ctrl_c_restores_terminal_after_failed_open(self):
        self.start(True)
        self.wait_text("Choose an Adapter")
        os.write(self.master, b"\x03")
        self.exited(0)
        self.assertIn(b"\x1b[?1003h", self.transcript)
        self.assertIn(b"\x1b[?1049l", self.transcript)

    def test_tui_external_signals_restore_terminal(self):
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig):
                self.start(True)
                self.wait_text("Choose an Adapter")
                self.process.send_signal(sig)
                self.exited(0)
                self.doCleanups()

    def test_tui_help_and_resize(self):
        import fcntl
        import termios
        self.start(True)
        self.wait_text("Choose an Adapter")
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
        os.write(self.master, b"unfinished\x03")
        self.exited(0)
        self.doCleanups()
        self.start(False)
        self.wait_text("Open failed:")
        self.process.send_signal(signal.SIGINT)
        self.exited(0)


if __name__ == "__main__":
    unittest.main()
