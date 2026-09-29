"""Offline readiness checks for the radio test tool; no hardware access."""
import importlib.util
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

tools = Path(__file__).parents[1] / "tools"
spec = importlib.util.spec_from_file_location("check_radio", tools / "check_radio.py")
radio = importlib.util.module_from_spec(spec)
with patch.object(sys, "path", [str(tools), *sys.path]):
    spec.loader.exec_module(radio)

STATUS = dict(adapter_id="adapter", boot_id="boot", session_id="session",
              radio_ready=True, storage_ready=True)


def reply(result, done=True, request=1):
    return dict(type="response", id=request, ok=True, done=done, result=result)


class RadioReadiness(unittest.TestCase):
    def run_ready(self, messages):
        now, sent = [0], []
        messages = iter(messages)

        def read(timeout):
            message = next(messages)
            if message is None:
                now[0] += timeout
                raise TimeoutError("silent adapter")
            if isinstance(message, Exception):
                raise message
            return message

        console = SimpleNamespace(id=0, ok=Mock(return_value={}), read=read,
                                  write=lambda data: sent.append(json.loads(data)))
        with patch.object(radio.time, "monotonic", side_effect=lambda: now[0]):
            status = radio.wait_ready(console, STATUS)
        console.ok.assert_called_once_with("session.heartbeat")
        return status, [request["cmd"] for request in sent]

    def test_immediate(self):
        status, commands = self.run_ready([reply(dict(state="ready", status=STATUS))])
        self.assertEqual(status, STATUS)
        self.assertEqual(commands, ["adapter.wait_ready"])

    def test_wait_renews_heartbeat_and_drains_reply(self):
        status, commands = self.run_ready([
            reply(dict(state="initializing"), done=False), None,
            reply(dict(state="ready", status=STATUS)), reply({}, request=2)])
        self.assertEqual(status, STATUS)
        self.assertEqual(commands, ["adapter.wait_ready", "session.heartbeat"])

    def test_invalid_or_failed_response(self):
        waiting = reply(dict(state="initializing"), done=False)
        for messages in ([waiting, waiting],
                         [reply(dict(state="ready", status=dict(STATUS, session_id="other")))],
                         [dict(type="response", id=1, ok=False, done=True,
                               error=dict(code="storage_failed"))]):
            with self.subTest(messages=messages), self.assertRaises(AssertionError):
                self.run_ready(messages)

    def test_deadline_and_disconnect(self):
        with self.assertRaises(TimeoutError):
            self.run_ready([None] * 7)
        with self.assertRaises(EOFError):
            self.run_ready([EOFError("USB disconnected")])
