"""Check Debian shared library dependency constraints."""
import unittest

from check_linux_package import check_debian_dependencies


class PackageMetadata(unittest.TestCase):
    def test_debian_library_minimums_accept_stronger_declared_bounds(self):
        required = "libadwaita-1-0 (>= 1.5~beta), libgtk-4-1 (>= 4.12.0), libudev1"
        for declared in (
                "libadwaita-1-0 (>= 1.5), libgtk-4-1 (>= 4.14), libudev1",
                "libadwaita-1-0 (= 1.5-1), libgtk-4-1 (>> 4.12.0), libudev1 (>= 1)",
                required):
            with self.subTest(declared=declared):
                check_debian_dependencies(required, declared)
        check_debian_dependencies("library (>= 1:1.0-2)", "library (>= 2:0.1-1)")
        check_debian_dependencies("library (>> 1.5)", "library (>> 1.5)")

    def test_debian_library_minimums_reject_missing_and_weaker_bounds(self):
        for declared in ("", "other (>= 4.14)", "libgtk-4-1", "libgtk-4-1 (>= 4.11)",
                         "libgtk-4-1 (= 4.11)", "libgtk-4-1 (<= 4.14)",
                         "libgtk-4-1 (>= 4.14) | alternative"):
            with self.subTest(declared=declared), self.assertRaises(ValueError):
                check_debian_dependencies("libgtk-4-1 (>= 4.12.0)", declared)
        with self.assertRaises(ValueError):
            check_debian_dependencies("libadwaita-1-0 (>= 1.5)", "libadwaita-1-0 (>= 1.5~beta)")
        with self.assertRaises(ValueError):
            check_debian_dependencies("library (>> 1.5)", "library (>= 1.5)")
