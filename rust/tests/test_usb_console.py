"""USB hardware checker must multiplex monitor events with command replies."""
import sys
import unittest
from pathlib import Path
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from check_usb import Console


class ConsoleTests(unittest.TestCase):
    def test_notifications_before_response(self):
        console = Console.__new__(Console)
        console.id = 0
        console.write = Mock()
        reply = {"type": "response", "id": 1, "done": True, "ok": True}
        console.read = Mock(side_effect=[
            {"type": "event", "event": "device.changed"},
            {"type": "event", "event": "events.lost"}, reply])
        self.assertEqual(console.command("adapter.status"), reply)
        console.read = Mock(return_value={"type": "event", "event": "protocol.error"})
        with self.assertRaises(AssertionError):
            console.command("adapter.status")
