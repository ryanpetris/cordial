"""Check the editor geometry against the firmware's usage-selector matrix."""
import json
import re
import unittest
from generate_vial_definition import ROOT, generate

CONFIGURATOR = ROOT / "rust/crates/cordial-core/src/configurator.rs"


def firmware_matrix():
    """The firmware's matrix size and the number of leading positions `input()` maps."""
    source = CONFIGURATOR.read_text()
    rows, cols = (int(re.search(rf"pub const {name}: usize = (\d+);", source)[1]) for name in ("ROWS", "COLS"))
    body = source[source.index("pub fn input(") :]
    body = body[: body.index("\n}\n")]
    used = max(int(end) for end in re.findall(r"\d+\.\.(\d+) =>", body))
    return rows, cols, used


class KeyboardDefinition(unittest.TestCase):
    def test_generated_definition_and_matrix(self):
        self.assertEqual(generate(), (ROOT / "rust/crates/cordial-core/src/vial-definition.xz").read_bytes())
        rows, cols, used = firmware_matrix()
        self.assertLessEqual(used, rows * cols)
        self.assertGreater(used, (rows - 1) * cols)
        layouts = []
        for name in ("via", "vial"):
            data = json.loads((ROOT / f"configs/keyboards/cordial-{name}.json").read_text())
            self.assertEqual((data["matrix"]["rows"], data["matrix"]["cols"]), (rows, cols))
            coordinates = [tuple(map(int, key.split(","))) for row in data["layouts"]["keymap"] for key in row]
            self.assertEqual(sorted(coordinates), [divmod(index, cols) for index in range(used)])
            layouts.append(data["layouts"])
        self.assertEqual(*layouts)
