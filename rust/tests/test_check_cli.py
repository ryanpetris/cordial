"""Terminal emulation used by tools/check_cli.py; no hardware access."""
import importlib.util
from pathlib import Path
import unittest

tools = Path(__file__).parents[1] / "tools"


@unittest.skipUnless(importlib.util.find_spec("pyte"), "check_cli.py needs pyte")
class ScrollRegion(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("check_cli", tools / "check_cli.py")
        cls.cli = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.cli)

    def screen(self, rows):
        screen = self.cli.Screen(40, len(rows))
        stream = self.cli.Stream(screen)
        stream.feed("\r\n".join(rows).encode())
        return screen, stream

    def test_scroll_down_and_up_stay_within_margins(self):
        screen, stream = self.screen([f"row {i}" for i in range(6)])
        stream.feed(b"\x1b[2;5r\x1b[4;7H\x1b[2T")
        self.assertEqual([line.rstrip() for line in screen.display],
                         ["row 0", "", "", "row 1", "row 2", "row 5"])
        self.assertEqual((screen.cursor.x, screen.cursor.y), (6, 3))
        stream.feed(b"\x1b[3S")
        self.assertEqual([line.rstrip() for line in screen.display],
                         ["row 0", "row 2", "", "", "", "row 5"])
        stream.feed(b"\x1b[r\x1b[S")
        self.assertEqual([line.rstrip() for line in screen.display],
                         ["row 2", "", "", "", "row 5", ""])

    def test_rows_moved_by_the_renderer_keep_their_controls(self):
        # As the TUI renderer inserts a status line above the settings page's
        # controls: it scrolls them down, then writes only the new line.
        rows = ["│ Battery   80", "│", "│ [Refresh] [Apply]  [‹ Back]", "│", "│", "╰──"]
        move = b"\x1b[3;5r\x1b[3;1H\x1b[1T\x1b[1;6r\x1b[3;1H\xe2\x94\x82 \xe2\x97\x8f Read current values: 4 read"
        screen, stream = self.screen(rows)
        stream.feed(move)
        self.assertEqual(screen.display[2].rstrip(), "│ ● Read current values: 4 read")
        self.assertEqual(screen.display[3].rstrip(), "│ [Refresh] [Apply]  [‹ Back]")
        # Without SU and SD, as in pyte itself, the controls are overwritten.
        plain = self.cli.pyte.Screen(40, len(rows))
        self.cli.pyte.ByteStream(plain).feed("\r\n".join(rows).encode() + move)
        self.assertFalse(any("[‹ Back]" in line for line in plain.display))


if __name__ == "__main__":
    unittest.main()
