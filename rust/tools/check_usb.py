#!/usr/bin/env python3
"""Linux development-board control checks; no Bluetooth pairing or HID injection.

Requires an explicit serial port. --bootloader MODE ends in ROM programming mode.
This script never flashes an image.
"""
import argparse
import fcntl
import json
import os
import select
import struct
import termios
import time


class Console:
    def __init__(self, port):
        self.fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        self.buffer = b""
        self.id = 0
        try:
            fcntl.ioctl(self.fd, termios.TIOCEXCL)
            attrs = termios.tcgetattr(self.fd)
            attrs[0] = attrs[1] = attrs[3] = 0
            attrs[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
            attrs[4] = attrs[5] = termios.B115200
            attrs[6][termios.VMIN] = attrs[6][termios.VTIME] = 0
            termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
            self.new_session()
        except BaseException:
            os.close(self.fd)
            raise

    def dtr(self, value):
        fcntl.ioctl(self.fd, termios.TIOCMBIS if value else termios.TIOCMBIC, struct.pack("I", termios.TIOCM_DTR))

    def new_session(self):
        self.dtr(False)
        time.sleep(0.05)
        termios.tcflush(self.fd, termios.TCIOFLUSH)
        self.dtr(True)
        self.buffer = b""
        self.id = 0
        self.read_line()  # Fresh-session LF also terminates any old USB fragment.

    def write(self, data):
        deadline = time.monotonic() + 3
        while data:
            if time.monotonic() >= deadline:
                raise TimeoutError("USB write timed out")
            if select.select([], [self.fd], [], 0.1)[1]:
                data = data[os.write(self.fd, data):]

    def read_line(self, timeout=3):
        deadline = time.monotonic() + timeout
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.fd], [], [], remaining)[0]:
                raise TimeoutError("USB response timed out")
            data = os.read(self.fd, 4096)
            if not data:
                raise EOFError("USB disconnected")
            self.buffer += data
            if len(self.buffer) > 8192:
                raise ValueError("Unbounded USB output")
        line, self.buffer = self.buffer.split(b"\n", 1)
        assert len(line) < 4096
        return line

    def read(self, timeout=3):
        line = self.read_line(timeout)
        while not line:
            line = self.read_line(timeout)
        message = json.loads(line)
        assert message["v"] == 1
        return message

    def command(self, cmd, args=None):
        self.id += 1
        self.write(json.dumps(dict(v=1, id=self.id, cmd=cmd, args=args or {})).encode() + b"\n")
        deadline = time.monotonic() + 3
        while True:
            reply = self.read(max(0, deadline - time.monotonic()))
            if reply["type"] == "event" and reply["event"] != "protocol.error":
                continue  # Monitoring notifications can precede this response.
            assert reply["type"] == "response" and reply["id"] == self.id and reply["done"], reply
            return reply

    def ok(self, cmd, args=None):
        reply = self.command(cmd, args)
        assert reply["ok"], reply
        return reply["result"]

    def close(self):
        try:
            self.dtr(False)
        except OSError:
            pass
        os.close(self.fd)


def main():
    if not __debug__:
        raise SystemExit("Run without -O: this hardware check requires assertions enabled.")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--adapter-id", required=True, help="expected running firmware adapter ID from USB enumeration")
    parser.add_argument("--hardware", default="pico_w")
    parser.add_argument("--bootloader", choices=("bootsel", "download"),
                        help="enter and check the expected ROM mode: bootsel for Pico, download for ESP32-S3")
    args = parser.parse_args()
    console = Console(args.port)
    try:
        initial = console.ok("adapter.status")
        assert initial["adapter_id"] == args.adapter_id
        assert initial["hardware_config"] == args.hardware
        assert initial["build_profile"] == "development"
        capabilities = console.ok("adapter.capabilities")
        assert "debug" in capabilities and "storage_management" in capabilities
        assert not initial["monitor"]
        console.ok("session.heartbeat")
        print(json.dumps(initial, indent=2), flush=True)
        console.write(b"\n\r\n")
        console.ok("adapter.status")  # Blank lines must not inject errors ahead of this reply.
        console.write(b'{\n')
        error = console.read(); assert error["event"] == "protocol.error" and error["data"]["code"] == "invalid_json"
        console.id += 1
        console.write(('{{"v":1,"v":1,"id":{},"cmd":"adapter.status","args":{{}}}}\n'.format(console.id)).encode())
        error = console.read(); assert error["event"] == "protocol.error" and error["data"]["code"] == "invalid_request"
        console.id += 1
        console.write(('{{"v":1,"id":{},"cmd":"device.list","args":{{"filter":"paired","filter":"connected"}}}}\n'.format(console.id)).encode())
        error = console.read(); assert error["id"] == console.id and not error["ok"] and error["error"]["code"] == "invalid_args"
        console.write(b"x" * 4096 + b"\n")
        error = console.read(); assert error["data"]["code"] == "message_too_large"
        console.ok("session.heartbeat")
        console.ok("session.monitor.set", {"enabled": True})
        assert console.ok("adapter.status")["monitor"]
        print("Waiting for the 15-second heartbeat expiry...", flush=True)
        time.sleep(15.2)
        expired = console.ok("adapter.status")
        assert not expired["monitor"] and expired["heartbeat"]["remaining_ms"] == 0
        assert console.command("session.monitor.set", {"enabled": True})["error"]["code"] == "heartbeat_required"
        assert not console.ok("session.heartbeat")["monitor"]
        # Baud changes are metadata only; they must never reboot the adapter.
        for speed in (termios.B1200, termios.B9600, termios.B115200):
            attrs = termios.tcgetattr(console.fd); attrs[4] = attrs[5] = speed
            termios.tcsetattr(console.fd, termios.TCSANOW, attrs)
            assert console.ok("adapter.status")["boot_id"] == initial["boot_id"]
        console.ok("session.monitor.set", {"enabled": True})
        console.new_session()
        reopened = console.ok("adapter.status")
        assert reopened["boot_id"] == initial["boot_id"] and reopened["session_id"] != initial["session_id"]
        assert not reopened["monitor"]
        console.ok("session.heartbeat")
        if args.bootloader:
            reply = console.ok("adapter.bootloader.enter")
            assert reply == {"rebooting": True, "mode": args.bootloader}
            print(f"{args.bootloader} acknowledged; verify ROM USB enumeration next.", flush=True)
        print("Control checks passed without pairing or input injection.", flush=True)
    finally:
        console.close()


if __name__ == "__main__":
    main()
