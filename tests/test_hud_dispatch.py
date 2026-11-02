"""Both live notification and setup probe use the same narrowly addressed registry API call."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("hud_registry_dispatch", Path(__file__).resolve().parents[1] / "tools/hud_registry_dispatch.py")
dispatch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dispatch)


class RegistryDispatch(unittest.TestCase):
    def test_release_preserves_pin_and_uses_exact_app_asset(self):
        pin = {"app_version": "0.10.0", "development": False, "sha256": "a" * 64}
        with patch.object(dispatch.subprocess, "run") as run:
            dispatch.dispatch(pin, "v0.10.0")
        call = run.call_args
        self.assertEqual(call.args[0], ["gh", "api", "repos/GLinnik21/plxnative-huds/dispatches", "--input", "-"])
        body = json.loads(call.kwargs["input"])
        self.assertEqual(body["event_type"], "app-release")
        self.assertEqual(body["client_payload"]["simulator"]["sha256"], pin["sha256"])
        self.assertIn("/v0.10.0/plxnative-hud-simulator-linux-x86_64.tar.gz", body["client_payload"]["simulator"]["url"])
        self.assertNotIn("url", pin)
        self.assertTrue(call.kwargs["check"])

    def test_probe_cannot_be_mistaken_for_an_app_release(self):
        with patch.object(dispatch.subprocess, "run") as run:
            dispatch.dispatch(probe="b" * 40)
        body = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(body, {"event_type": "app-release-probe", "client_payload": {"source_sha": "b" * 40}})

    def test_bad_tag_development_pin_and_mixed_probe_fail_before_network(self):
        cases = [({"app_version": "0.10.0"}, "v0.11.0", ""),
                 ({"app_version": "0.10.0", "development": True}, "v0.10.0", ""),
                 (None, "", "short"), ({}, "", "c" * 40), (None, "v0.10.0", "d" * 40)]
        with patch.object(dispatch.subprocess, "run") as run:
            for pin, tag, probe in cases:
                with self.subTest(pin=pin, tag=tag, probe=probe), self.assertRaises(ValueError):
                    dispatch.dispatch(pin, tag, probe)
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
