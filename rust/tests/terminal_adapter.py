"""Serial PTY fixture with real framing and generated protocol messages."""
import atexit
from functools import cache
import importlib.util
import os
from pathlib import Path
import pty
import queue
import select
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[2]


def encode(data):
    result = bytearray([0])
    at, code = 0, 1
    for byte in data:
        if byte == 0:
            result[at] = code
            at, code = len(result), 1
            result.append(0)
        else:
            result.append(byte)
            code += 1
            if code == 255:
                result[at] = code
                at, code = len(result), 1
                result.append(0)
    result[at] = code
    return result + b"\0"


def decode(data):
    result = bytearray()
    at = 0
    while at < len(data):
        code = data[at]
        result.extend(data[at + 1:at + code])
        at += code
        if code < 255 and at < len(data):
            result.append(0)
    return bytes(result)


@cache
def _artifacts():
    temporary = tempfile.TemporaryDirectory(prefix="cordial-terminal-")
    directory = Path(temporary.name)
    version = (ROOT / "proto/serial/current").read_text().strip()
    schema = ROOT / "proto/serial" / f"v{version}"
    subprocess.run(["protoc", f"-I{schema}", f"-I{ROOT / 'proto'}",
                    f"--python_out={directory}", str(schema / "cordial.proto"),
                    str(ROOT / "proto/handshake.proto")], check=True, capture_output=True)

    def module(name):
        spec = importlib.util.spec_from_file_location(name, directory / f"{name}.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    protocol = module("cordial_pb2")
    handshake = module("handshake_pb2")
    # PTYs support serial byte transport but have no modem-control lines.
    shim = directory / "modem.c"
    shim.write_text('''#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <sys/ioctl.h>
int ioctl(int fd, unsigned long request, ...) {
    va_list args;
    va_start(args, request);
    void *arg = va_arg(args, void *);
    va_end(args);
    if (request == TIOCMBIS || request == TIOCMBIC) return 0;
    if (request == TIOCMGET) { *(int *)arg = TIOCM_DTR; return 0; }
    int (*original)(int, unsigned long, ...) = dlsym(RTLD_NEXT, "ioctl");
    return original(fd, request, arg);
}
''')
    library = directory / "modem.so"
    subprocess.run(["cc", "-shared", "-fPIC", "-o", str(library), str(shim), "-ldl"],
                   check=True, capture_output=True)
    atexit.register(temporary.cleanup)
    return protocol, handshake, int(version), library


class Adapter:
    def __init__(self):
        self.p, handshake, version, library = _artifacts()
        self.hello = handshake.AdapterHello(protocol_version=version)
        self.env = dict(os.environ, LD_PRELOAD=str(library))
        self.master, self.slave = pty.openpty()
        self.port = os.ttyname(self.slave)
        self.stop = threading.Event()
        self.error = None
        self.sent = []
        self.events = queue.SimpleQueue()
        self.data = bytes(n % 256 for n in range(1537))
        self.status = self.p.Status(id="ADAPTER01", name="Terminal Adapter", ready=True)
        self.status.information.development_build = True
        self.status.information.storage_format = 2
        self.status.information.target_storage_format = 2
        self.thread = threading.Thread(target=self.run)
        self.thread.start()

    def close(self):
        self.stop.set()
        self.thread.join(timeout=3)
        os.close(self.master)
        os.close(self.slave)
        assert not self.thread.is_alive(), "serial fixture did not stop"
        if self.error:
            raise self.error

    def run(self):
        try:
            pending = bytearray()
            while not self.stop.is_set():
                while not self.events.empty():
                    self.send(self.events.get_nowait())
                if not select.select([self.master], [], [], .05)[0]:
                    continue
                pending.extend(os.read(self.master, 65536))
                while 0 in pending:
                    frame, _, pending = pending.partition(b"\0")
                    if frame:
                        self.answer(decode(frame))
        except Exception as error:
            self.error = error

    def send(self, message):
        self.sent.append(message)
        data = encode(message.SerializeToString())
        while data and not self.stop.is_set():
            if select.select([], [self.master], [], .05)[1]:
                count = os.write(self.master, data[:1024])
                data = data[count:]

    def answer(self, data):
        if not data:
            os.write(self.master, encode(self.hello.SerializeToString()))
            return
        request = self.p.Request.FromString(data)
        kind = request.WhichOneof("command")
        reply = self.p.Message()
        if kind == "get_status":
            reply.response.status.CopyFrom(self.status)
        elif kind == "list_devices":
            reply.response.devices.SetInParent()
        elif kind == "list_profiles":
            reply.response.profiles.SetInParent()
        elif kind == "read_file":
            file = request.read_file
            chunk = self.data[file.offset:]
            if file.HasField("length"):
                chunk = chunk[:file.length]
            reply.response.file.CopyFrom(self.p.FileData(
                path=file.path, offset=file.offset, size=len(self.data), data=chunk))
        else:
            raise AssertionError(f"unexpected serial request: {kind}")
        if kind == "get_status":
            # The response lets the command finish, so send its event first to guarantee
            # receipt even when the next script line closes the connection.
            self.send(self.p.Message(event=self.p.Event(adapter=self.status)))
        self.send(reply)
