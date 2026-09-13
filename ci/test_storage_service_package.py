"""Exercise actual service archive metadata for both install identities, without an NDK."""
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import mkipk


class StorageServicePackage(unittest.TestCase):
    def stage_app(self, data, app):
        appdir = data / "usr/palm/applications" / app["id"]
        appdir.mkdir(parents=True)
        (appdir / "appinfo.json").write_text(json.dumps({**app, "requiredPermissions": mkipk.STORAGE_PERMISSIONS}))

    def test_built_helper_packages_for_both_flavors_when_available(self):
        repo = Path(__file__).resolve().parent.parent
        if not (repo / "pkg/plxnative-storage").is_file():
            self.skipTest("cross-built helper not available")
        for flav in ("stable", "debug"):
            with self.subTest(flavor=flav), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                app = mkipk.flavor.appinfo_for(flav)
                self.assertEqual(app["requiredPermissions"], ["database.operation", "securitykey.operation"])
                data = root / "data"
                self.stage_app(data, app)
                mkipk.stage_storage_service(repo, data, app)
                mkipk.write_packageinfo(data, app)
                mkipk.write_targz(root / "data.tar.gz", data, "")
                self.assertEqual(mkipk.storage_archive_errors((root / "data.tar.gz").read_bytes(), app["id"]), [])

    def test_both_flavors_have_private_native_service_and_no_writable_state(self):
        for app_id in ("com.beb.plxnative", "com.beb.plxnative.debug"):
            with self.subTest(app_id=app_id), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "pkg").mkdir()
                (root / "pkg/plxnative-storage").write_bytes(b"\x7fELF\x01\x01" + bytes(12) + b"\x28\x00")
                app = {"id": app_id, "title": "PlxNative", "vendor": "PlxNative", "version": "0.6.6"}
                data = root / "data"
                self.stage_app(data, app)
                mkipk.stage_storage_service(root, data, app)
                mkipk.write_packageinfo(data, app)
                archive = root / "data.tar.gz"
                mkipk.write_targz(archive, data, "")
                self.assertEqual(mkipk.storage_archive_errors(archive.read_bytes(), app_id), [])
                first = archive.read_bytes()
                mkipk.write_targz(archive, data, "")
                self.assertEqual(first, archive.read_bytes())
                with tarfile.open(fileobj=io.BytesIO(archive.read_bytes()), mode="r:gz") as tf:
                    service = json.load(tf.extractfile(f"usr/palm/services/{app_id}.storage/services.json"))
                    self.assertEqual(service["services"], [{"name": app_id + ".storage", "commands": []}])
                    package = json.load(tf.extractfile(f"usr/palm/packages/{app_id}/packageinfo.json"))
                    self.assertEqual(package["services"], [app_id + ".storage"])
                    self.assertEqual(package["requiredPermissions"], ["database.operation", "securitykey.operation"])
                    application = json.load(tf.extractfile(f"usr/palm/applications/{app_id}/appinfo.json"))
                    self.assertEqual(application["requiredPermissions"], package["requiredPermissions"])
                    self.assertFalse(any("/state" in m.name for m in tf.getmembers()))

    def test_archive_checker_rejects_obsolete_state_and_foreign_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "usr/palm/applications/com.beb.plxnative/state").mkdir(parents=True)
            foreign = root / "usr/palm/services/com.beb.plxnative.debug.storage"
            foreign.mkdir(parents=True)
            (foreign / "auth.json").write_text('{"token":"synthetic-fixture"}')
            archive = root.parent / (root.name + ".tar.gz")
            try:
                mkipk.write_targz(archive, root, "")
                errors = mkipk.storage_archive_errors(archive.read_bytes(), "com.beb.plxnative")
                self.assertIn("obsolete writable app state is forbidden", errors)
                self.assertIn("runtime credentials or rendezvous files must not be packaged", errors)
                self.assertIn("service payload must contain exactly this flavor's descriptor and helper", errors)
            finally:
                archive.unlink(missing_ok=True)

    def test_missing_helper_is_a_build_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(SystemExit):
                mkipk.stage_storage_service(Path(tmp), Path(tmp) / "data", {"id": "com.beb.plxnative"})


if __name__ == "__main__":
    unittest.main()
