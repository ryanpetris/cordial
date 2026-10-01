import pathlib
import sys
import tomllib
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import check_keys


def catalog(*entries, rows=()):
    return {"key": list(entries), "row": list(rows)}


def key(name, lists=("device",), kind="integer", **extra):
    return {"key": name, "lists": list(lists), "type": kind, **extra}


class Problems(unittest.TestCase):
    def test_checked_in_catalog_is_valid(self):
        text = (check_keys.ROOT / check_keys.CATALOG).read_text()
        self.assertEqual(check_keys.problems(tomllib.loads(text)), [])

    def test_naming_rules(self):
        found = check_keys.problems(
            catalog(
                key("single"),
                key("{n}.level"),
                key("battery.Level"),
                key("vendor.id_"),
                key("wheel.diameter.mm"),
                key("wheel.diameter"),
            )
        )
        self.assertIn("single: needs at least two levels", found)
        self.assertIn("{n}.level: the first level names a part, not an index", found)
        self.assertTrue(any("battery.Level" in p for p in found))
        self.assertTrue(any("vendor.id_" in p for p in found))
        self.assertIn("wheel.diameter is a prefix of wheel.diameter.mm", found)

    def test_template_prefixes(self):
        found = check_keys.problems(
            catalog(key("pointer.sensor.{n}.dpi"), key("pointer.sensor.0.dpi.x"))
        )
        self.assertIn("pointer.sensor.{n}.dpi is a prefix of pointer.sensor.0.dpi.x", found)

    def test_types_and_values(self):
        found = check_keys.problems(
            catalog(
                key("a.b", kind="enum"),
                key("c.d", kind="bool", values=["x"]),
                key("e.f", kind="text", unit="seconds"),
                key("g.h", lists=("host",)),
            )
        )
        self.assertIn("a.b: an enum needs unique values", found)
        self.assertIn("c.d: only enum keys have values", found)
        self.assertIn("e.f: only integer keys have a unit", found)
        self.assertTrue(any(p.startswith("g.h: lists") for p in found))

    def test_rows_name_settings(self):
        found = check_keys.problems(
            catalog(key("a.b"), rows=[{"keys": ["a.b", "missing.key"]}])
        )
        self.assertIn("row key a.b: not a setting key", found)
        self.assertIn("row key missing.key: not a setting key", found)


class Breaks(unittest.TestCase):
    def test_growth_is_allowed(self):
        old = catalog(key("a.b", kind="enum", values=["x"]))
        new = catalog(
            key("a.b", lists=("device", "setting"), kind="enum", values=["x", "y"]),
            key("c.d"),
        )
        self.assertEqual(check_keys.breaks(old, new), [])

    def test_retiring_is_allowed(self):
        old = catalog(key("a.b", kind="enum", values=["x", "y"]))
        new = catalog(key("a.b", kind="enum", values=["x"], retired_values=["y"], retired=True))
        self.assertEqual(check_keys.breaks(old, new), [])

    def test_breaking_changes(self):
        old = catalog(
            key("a.b"),
            key("c.d", kind="enum", values=["x", "y"], retired_values=["z"]),
            key("e.f", lists=("device", "setting"), unit="seconds"),
        )
        new = catalog(
            key("c.d", kind="enum", values=["x", "z"]),
            key("e.f", unit="milliseconds"),
        )
        found = check_keys.breaks(old, new)
        self.assertIn("a.b: removed; mark it retired instead", found)
        self.assertIn("c.d: value y removed; list it in retired_values instead", found)
        self.assertIn("c.d: retired value z reused", found)
        self.assertIn("e.f: no longer listed in setting", found)
        self.assertIn("e.f: unit changed from seconds to milliseconds", found)


if __name__ == "__main__":
    unittest.main()
