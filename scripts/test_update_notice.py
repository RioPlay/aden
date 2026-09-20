#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import unittest
from update_notice import render_notice


class NoticeSafetyTests(unittest.TestCase):
    def test_preserves_maintained_notices_and_separate_versions(self):
        preamble = "# NOTICE\nMaintained copyright and vendored license text.\n\n"
        packages = [{"name": "dep", "version": version, "source": "registry+example",
                     "license": "MIT OR Apache-2.0", "repository": "https://example.invalid"}
                    for version in ("2.0.0", "1.0.0")]
        packages.append({"name": "local", "source": None})
        result = render_notice(preamble + "# Third-Party Dependencies\nstale\n",
                               {"packages": packages})
        self.assertTrue(result.startswith(preamble))
        self.assertIn("### dep v1.0.0", result)
        self.assertIn("### dep v2.0.0", result)
        self.assertNotIn("### local", result)
        self.assertNotIn("stale", result)
        self.assertEqual(result, render_notice(result, {"packages": packages}))

    def test_refuses_missing_license_or_incomplete_input(self):
        original = "# NOTICE\n# Third-Party Dependencies\n"
        for packages in ([], [{"name": "dep", "version": "1", "source": "registry+x"}]):
            with self.assertRaises(ValueError):
                render_notice(original, {"packages": packages})
        with self.assertRaises(ValueError):
            render_notice("No legal section marker", {"packages": []})


if __name__ == "__main__":
    unittest.main()
