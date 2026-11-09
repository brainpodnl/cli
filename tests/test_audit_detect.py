import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import audit_detect


class DetectionAuditTests(unittest.TestCase):
    def setUp(self):
        self.project = {
            "path": ".",
            "names": ["app"],
            "languages": [
                {
                    "name": "javascript",
                    "versions": [{"value": ">=22", "source": "package.json"}],
                }
            ],
            "detections": [{"id": "node"}, {"id": "node-workspace"}],
            "workspaceMembers": ["packages/api"],
        }
        self.entry = {"repository": "owner/repo", "revision": "a" * 40}

    def test_checks_are_scoped_to_the_expected_project(self):
        cases = [
            {"kind": "language", "language": "javascript"},
            {"kind": "detection", "id": "node"},
            {"kind": "name", "name": "app"},
            {"kind": "workspace", "id": "node-workspace", "member": "packages/api"},
            {
                "kind": "version",
                "language": "javascript",
                "value": ">=22",
                "source": "package.json",
            },
        ]
        for case in cases:
            with self.subTest(case=case):
                self.assertTrue(
                    audit_detect.check({".": self.project}, {**case, "path": "."})[0]
                )
                self.assertFalse(
                    audit_detect.check(
                        {".": self.project}, {**case, "path": "missing"}
                    )[0]
                )
        wrong = {
            "kind": "version",
            "path": ".",
            "language": "javascript",
            "value": ">=22",
            "source": "wrong.json",
        }
        self.assertFalse(audit_detect.check({".": self.project}, wrong)[0])
        with self.assertRaises(ValueError):
            audit_detect.check({".": self.project}, {"kind": "unknown", "path": "."})

    def test_role_and_version_kind_expectations_are_enforced(self):
        self.project["detections"][0]["roles"] = ["uses"]
        expected = {
            "kind": "detection",
            "path": ".",
            "id": "node",
            "roles": ["provides"],
        }
        self.assertFalse(audit_detect.check({".": self.project}, expected)[0])
        expected["roles"] = ["uses"]
        self.assertTrue(audit_detect.check({".": self.project}, expected)[0])
        self.project["languages"][0]["versions"][0]["kind"] = "sdk"
        expected = {
            "kind": "version",
            "path": ".",
            "language": "javascript",
            "value": ">=22",
            "source": "package.json",
            "versionKind": "language",
        }
        self.assertFalse(audit_detect.check({".": self.project}, expected)[0])
        expected["versionKind"] = "sdk"
        self.assertTrue(audit_detect.check({".": self.project}, expected)[0])

    def archive(self, path="repo/package.json", size=None):
        data = b"{}" if size is None else b"x" * size
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            member = tarfile.TarInfo(path)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
            link = tarfile.TarInfo("repo/linked.json")
            link.type = tarfile.SYMTYPE
            link.linkname = "/etc/passwd"
            archive.addfile(link)
        return output.getvalue()

    def test_missing_snapshots_never_download_without_fetch(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            patch("urllib.request.urlopen") as request,
        ):
            with self.assertRaisesRegex(ValueError, "pass --fetch"):
                audit_detect.snapshot(self.entry, Path(directory), False)
            request.assert_not_called()

    def test_cached_snapshot_requires_the_pinned_revision(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            patch("urllib.request.urlopen") as request,
        ):
            cache = Path(directory)
            destination = cache / "repos/owner--repo"
            destination.mkdir(parents=True)
            metadata = cache / "metadata/owner--repo.json"
            metadata.parent.mkdir()
            metadata.write_text(json.dumps(self.entry))
            self.assertEqual(
                audit_detect.snapshot(self.entry, cache, False), destination
            )
            metadata.write_text(json.dumps({**self.entry, "revision": "b" * 40}))
            with self.assertRaisesRegex(ValueError, "unverified snapshot exists"):
                audit_detect.snapshot(self.entry, cache, True)
            request.assert_not_called()

    def test_fetch_extracts_regular_files_but_not_symlinks(self):
        with (
            tempfile.TemporaryDirectory() as directory,
            patch("urllib.request.urlopen", return_value=io.BytesIO(self.archive())),
        ):
            destination = audit_detect.snapshot(self.entry, Path(directory), True)
            self.assertEqual((destination / "package.json").read_text(), "{}")
            self.assertFalse((destination / "linked.json").exists())
            self.assertFalse((destination / "linked.json").is_symlink())

    def test_unsafe_archives_do_not_escape_the_cache(self):
        for path in ["repo/../../escape", "/absolute/path"]:
            with (
                self.subTest(path=path),
                tempfile.TemporaryDirectory() as directory,
                patch(
                    "urllib.request.urlopen",
                    return_value=io.BytesIO(self.archive(path)),
                ),
            ):
                with self.assertRaisesRegex(ValueError, "unsafe archive path"):
                    audit_detect.snapshot(self.entry, Path(directory), True)
                self.assertFalse((Path(directory) / "repos/owner--repo").exists())

    def test_archive_and_expansion_limits_are_enforced(self):
        for limit, archive, message in [
            (1, self.archive(), "archive exceeds"),
            (512, self.archive(size=4096), "expanded snapshot exceeds"),
        ]:
            with (
                self.subTest(limit=limit),
                tempfile.TemporaryDirectory() as directory,
                patch("urllib.request.urlopen", return_value=io.BytesIO(archive)),
                patch.object(audit_detect, "MAX_ARCHIVE_BYTES", limit),
                self.assertRaisesRegex(ValueError, message),
            ):
                audit_detect.snapshot(self.entry, Path(directory), True)

    def test_invalid_repository_metadata_is_rejected(self):
        for entry in [
            {**self.entry, "repository": "../repo"},
            {**self.entry, "revision": "main"},
        ]:
            with (
                self.subTest(entry=entry),
                tempfile.TemporaryDirectory() as directory,
                self.assertRaisesRegex(ValueError, "invalid repository"),
            ):
                audit_detect.snapshot(entry, Path(directory), False)


if __name__ == "__main__":
    unittest.main()
