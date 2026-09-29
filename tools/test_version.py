"""Application versions supplied through the build environment."""
import os
import unittest
from unittest.mock import patch

from version import resolve


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
