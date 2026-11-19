#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import unittest
from update_notice import render_notice


class NoticeSafetyTests(unittest.TestCase):
    def test_preserves_maintained_notices_and_separate_versions(self):
        preamble = "# NOTICE\nMaintained copyright and vendored license text.\n\n"
        packages = [{"name": "dep", "version": version, "id": f"dep-{version}",
                     "source": "registry+example",
                     "license": "MIT OR Apache-2.0", "repository": "https://example.invalid"}
                    for version in ("2.0.0", "1.0.0")]
        packages.append({"name": "local", "version": "1.0.0", "id": "local-id",
                         "source": None})
        result = render_notice(preamble + "# Third-Party Dependencies\nstale\n",
                               {"packages": packages, "workspace_members": ["local-id"]})
        self.assertTrue(result.startswith(preamble))
        self.assertIn("### dep v1.0.0", result)
        self.assertIn("### dep v2.0.0", result)
        self.assertNotIn("### local", result)
        self.assertNotIn("stale", result)
        self.assertEqual(result, render_notice(result, {
            "packages": packages, "workspace_members": ["local-id"]
        }))

    def test_includes_external_path_dependency_but_excludes_workspace_crate(self):
        preamble = "# NOTICE\nMaintained license text.\n\n"
        workspace_id = "path+file:///repo#aden@0.1.0"
        vendor_id = "path+file:///repo/vendor/liquid-core#liquid-core@0.2.0"
        metadata = {
            "packages": [
                {"name": "aden", "version": "0.1.0", "id": workspace_id,
                 "source": None, "license": "AGPL-3.0-or-later"},
                {"name": "liquid-core", "version": "0.2.0", "id": vendor_id,
                 "source": None, "license": "MIT", "repository": "https://example.invalid/liquid-core"},
            ],
            "workspace_members": [workspace_id],
        }

        result = render_notice(preamble + "# Third-Party Dependencies\nstale\n", metadata)

        self.assertTrue(result.startswith(preamble))
        self.assertIn("### liquid-core v0.2.0", result)
        self.assertIn("- **License**: MIT", result)
        self.assertNotIn("### aden", result)
        self.assertNotIn("stale", result)

    def test_refuses_missing_license_or_incomplete_input(self):
        original = "# NOTICE\n# Third-Party Dependencies\n"
        for packages in ([], [{"name": "dep", "version": "1", "id": "dep-id",
                               "source": "registry+x"}]):
            with self.assertRaises(ValueError):
                render_notice(original, {"packages": packages, "workspace_members": []})
        with self.assertRaises(ValueError):
            render_notice("No legal section marker", {
                "packages": [], "workspace_members": []
            })


if __name__ == "__main__":
    unittest.main()
