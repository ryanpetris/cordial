#!/usr/bin/env python3
"""Linux PTY/SGR-mouse smoke check. Uses pyte; never pairs, flashes or injects HID."""
import argparse
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time

import pyte


class Screen(pyte.Screen):
    """pyte screen with SU and SD, which the TUI renderer uses to move rows
    within a scroll region. pyte ignores both."""

    def scroll(self, count, down):
        top, bottom = self.margins or pyte.screens.Margins(0, self.lines - 1)
        rows = [self.buffer[y] for y in range(top, bottom + 1)]
        count = min(count or 1, len(rows))
        rows = [None] * count + rows[:-count] if down else rows[count:] + [None] * count
        for y, row in enumerate(rows, top):
            if row is None:
                self.buffer.pop(y, None)
            else:
                self.buffer[y] = row
        self.dirty.update(range(top, bottom + 1))

    def scroll_up(self, count=1, *_, private=False):
        if not private:
            self.scroll(count, down=False)

    def scroll_down(self, count=1, *_, private=False):
        if not private:
            self.scroll(count, down=True)


class Stream(pyte.ByteStream):
    csi = {**pyte.ByteStream.csi, "S": "scroll_up", "T": "scroll_down"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--shell", action="store_true", help="check shell scrollback and command output")
    parser.add_argument("--heartbeat-expiry", action="store_true", help="pause the shell past its heartbeat lease and verify recovery")
    args = parser.parse_args()
    if args.heartbeat_expiry and not args.shell:
        parser.error("--heartbeat-expiry requires --shell")
    ports = json.loads(subprocess.check_output([args.binary, "--json", "adapter", "list"]))
    matches = [p for p in ports if p["serial"] == args.serial]
    assert len(matches) == 1, matches
    responses = subprocess.check_output(
        [args.binary, "--port", matches[0]["port"], "--json", "adapter", "status"],
        stderr=subprocess.DEVNULL, text=True)
    statuses = [row["result"] for line in responses.splitlines()
                if (row := json.loads(line)).get("result", {}).get("adapter_id") == args.serial]
    assert statuses, "status handshake missing"
    capability_output = subprocess.check_output(
        [args.binary, "--port", matches[0]["port"], "--json", "adapter", "capabilities"],
        stderr=subprocess.DEVNULL, text=True)
    capabilities = [row["result"] for line in capability_output.splitlines()
                    if isinstance((row := json.loads(line)).get("result"), list)]
    assert capabilities, "capability handshake missing"
    transports = set(capabilities[-1]) & {"classic", "ble"}
    scan_button, scan_started = {
        frozenset(("classic", "ble")): ("[Scan ▾]", "Scanning BLE + Classic"),
        frozenset(("classic",)): ("[Scan Classic]", "Scanning Classic"),
        frozenset(("ble",)): ("[Scan BLE]", "Scanning BLE"),
        frozenset(): (None, None),
    }[frozenset(transports)]
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    screen = Screen(100, 38)
    stream = Stream(screen)
    transcript = bytearray()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 100, 0, 0))
    env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
    command = [args.binary, "tui"]
    if args.shell:
        command = [args.binary, "--port", matches[0]["port"]]
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env)

    def read_for(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master], [], [], min(0.05, max(0, deadline - time.monotonic())))
            if not readable:
                continue
            data = os.read(master, 65536)
            transcript.extend(data)
            stream.feed(data)
            if b"\x1b[6n" in data:
                os.write(master, b"\x1b[1;1R")
            if b"\x1b[c" in data:
                os.write(master, b"\x1b[?1;2c")

    def locate(text):
        for y, line in enumerate(screen.display):
            if text in line:
                return line.index(text), y
        return None

    def wait_for(text, seconds=6):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            read_for(0.1)
            if locate(text) is not None:
                return
            if process.poll() is not None:
                break
        raise AssertionError(f"Missing {text!r}:\n" + "\n".join(screen.display))

    def click(text):
        point = locate(text)
        assert point is not None, (text, screen.display)
        x, y = point
        os.write(master, f"\x1b[<0;{x+2};{y+1}M\x1b[<0;{x+2};{y+1}m".encode())
        read_for(0.25)

    def hover(text):
        point = locate(text)
        assert point is not None, (text, screen.display)
        x, y = point
        os.write(master, f"\x1b[<35;{x+2};{y+1}M".encode())
        read_for(0.2)
        assert screen.buffer[y][x+1].reverse, f"hover did not highlight {text!r}"
        return x+1, y

    try:
        if args.shell:
            for _ in range(60):
                read_for(0.1)
                if b"Connected to" in transcript:
                    break
            assert b"Connected to" in transcript, "shell did not connect"
            read_for(0.5)
            assert b'"firmware_version"' not in transcript, "startup printed internal status JSON"
            assert b'"enabled":true' not in transcript, "startup printed monitor JSON"
            start = len(transcript)
            os.write(master, b"help\r")
            for _ in range(60):
                read_for(0.1)
                if b"can be shown or changed." in transcript[start:]:
                    break
            assert b"can be shown or changed." in transcript[start:], "help tail missing"
            assert b"List attached adapters" in transcript[start:], "help beginning lost"
            assert b"Close control session" in transcript[start:], "command list tail lost"
            start = len(transcript)
            os.write(master, b"adapter status\r")
            for _ in range(60):
                read_for(0.1)
                if b"Pending Operations:" in transcript[start:]:
                    break
            assert b"Storage Ready: yes" in transcript[start:], "status readiness missing"
            assert b"Pending Operations:" in transcript[start:], "status tail missing"
            assert b'"firmware_version"' not in transcript, "shell printed raw status JSON"
            if args.heartbeat_expiry:
                read_for(0.2)
                start = len(transcript)
                process.send_signal(signal.SIGSTOP)
                time.sleep(16)
                process.send_signal(signal.SIGCONT)
                for _ in range(80):
                    read_for(0.1)
                    output = transcript[start:]
                    if b"Monitoring expired" in output:
                        break
                    assert process.poll() is None, "shell exited instead of recovering monitoring"
                assert b"Monitoring expired" in output, "heartbeat expiry was not reported"
                # Recovery is quiet; explicitly request the state to verify it.
                for _ in range(10):
                    start = len(transcript)
                    os.write(master, b"adapter status\r")
                    for _ in range(60):
                        read_for(0.1)
                        if b"Pending Operations:" in transcript[start:]:
                            break
                    if b"Monitoring: on" in transcript[start:]:
                        break
                assert b"Monitoring: on" in transcript[start:], "monitoring/status did not recover"
                print("PTY heartbeat-expiry recovery passed after a 16-second process pause")
            os.write(master, b"quit\r")
            process.wait(timeout=3)
            read_for(0.1)
            assert process.returncode == 0
            assert termios.tcgetattr(slave) == original, "terminal attributes were not restored"
            print("PTY shell full help/status scrollback and terminal restoration passed")
            return
        if len(ports) == 1:
            wait_for("● Ready")
            assert locate("Choose an Adapter") is None, "single adapter was not opened at startup"
            click("[Adapter ▾]")
            click("Switch adapter…")
        wait_for("Choose an Adapter")
        assert b"\x1b[?1003h" in transcript, "all-motion mouse reporting not enabled"
        assert locate("Serial port path") is None, "manual path input remains in the TUI"
        assert locate("[Open port]") is None, "manual path button remains in the TUI"
        hover(args.serial)
        assert locate("Choose an Adapter") is not None, "hover activated an adapter"
        click("[Refresh]")
        read_for(0.5)
        assert locate("Choose an Adapter") is not None, "Refresh repeated startup auto-connect"
        # Listed adapters still support an explicit mouse selection.
        click(args.serial)
        wait_for("● Ready")
        if scan_button:
            wait_for(scan_button)
        x, y = hover("[Help]")
        assert locate("[Close]") is None, "hover opened Help"
        os.write(master, f"\x1b[<35;1;{screen.lines}M".encode())
        read_for(0.2)
        assert not screen.buffer[y][x].reverse, "hover stayed highlighted after moving away"
        if len(transports) < 2:
            assert locate("[Scan ▾]") is None, "combined scan offered without both transports"
        if scan_button:
            hover(scan_button)
            assert locate("Bluetooth LE and Classic") is None, "hover opened the Scan menu"
            click(scan_button)
            if len(transports) == 2:
                click("Bluetooth LE and Classic")
            wait_for(scan_started)
            read_for(1)
            click("[Stop scan]")
            wait_for("Scan stopped")
        else:
            assert locate("[Scan") is None, "scan offered without transport/command support"
        click("[Adapter ▾]")
        click("Pause live updates")
        wait_for("Live updates off")
        click("[Adapter ▾]")
        click("Resume live updates")
        wait_for("● Ready")
        click("[Help]")
        wait_for("Indicators")
        for _ in range(40):
            if locate("Mouse and Keyboard") is not None:
                break
            os.write(master, b"\x1b[<65;50;20M")
            read_for(0.1)
        wait_for("Mouse and Keyboard")
        click("[Close]")
        if scan_button:
            wait_for(scan_button)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 55, 0, 0))
        screen.resize(24, 55)
        process.send_signal(signal.SIGWINCH)
        read_for(0.5)
        wait_for("[Quit]")
        click("[Quit]")
        process.wait(timeout=3)
        read_for(0.1)
        assert process.returncode == 0, process.returncode
        restored = termios.tcgetattr(slave)
        assert restored == original, "terminal attributes were not restored"
        assert b"\x1b[?1006l" in transcript, "SGR mouse reporting not disabled"
        assert b"\x1b[?1003l" in transcript, "all-motion mouse reporting not disabled"
        assert b"\x1b[?1049l" in transcript, "alternate screen not restored"
        print("PTY startup connection, hover, mouse chooser/discovery/monitor/help/resize/quit and terminal restoration passed")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    main()
