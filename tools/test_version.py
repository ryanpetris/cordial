"""Application versions supplied through the build environment."""
import os
import unittest
from unittest.mock import patch

from version import protocol_base, resolve


class Versions(unittest.TestCase):
    def test_default_and_explicit_versions(self):
        with patch.dict(os.environ):
            os.environ.pop("CORDIAL_VERSION", None)
            self.assertEqual(resolve(), "0.0.0")
            for value in ("0.0.0", "1.2.3", "12.34.56"):
                os.environ["CORDIAL_VERSION"] = value
                self.assertEqual(resolve(), value)
            for value in ("", "v1.2.3", "01.2.3", "1.2", "1.2.3\n", "../1.2.3", "1.2.3-rc1"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    os.environ["CORDIAL_VERSION"] = value
                    resolve()


class ProtocolBase(unittest.TestCase):
    TAGS = ["v0.4.0", "v0.5.0", "v0.5.3", "v0.6.0", "v0.6.2", "v1.0.0", "v1.2.0", "v2.0.0", "other"]

    def test_before_1_0_a_minor_version_starts_a_new_series(self):
        self.assertIsNone(protocol_base("0.7.0", ["v0.5.3", "v0.6.2"]))
        self.assertEqual(protocol_base("0.6.3", self.TAGS), "v0.6.2")
        self.assertEqual(protocol_base("0.6.1", self.TAGS), "v0.6.0")
        self.assertIsNone(protocol_base("0.6.0", self.TAGS))

    def test_from_1_0_only_a_major_version_starts_a_new_series(self):
        self.assertEqual(protocol_base("1.3.0", self.TAGS), "v1.2.0")
        self.assertEqual(protocol_base("1.0.1", self.TAGS), "v1.0.0")
        self.assertIsNone(protocol_base("1.0.0", self.TAGS))
        self.assertIsNone(protocol_base("3.0.0", self.TAGS))

    def test_a_development_build_compares_against_the_newest_release(self):
        self.assertEqual(protocol_base("0.0.0", self.TAGS), "v2.0.0")
        self.assertIsNone(protocol_base("0.0.0", []))
