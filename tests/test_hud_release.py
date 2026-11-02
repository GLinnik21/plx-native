"""Release artifact retries preserve existing bytes and do not label a dev simulator as shipped."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"tools"))
import hud_release


class Attachment(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory(); self.addCleanup(self.temporary.cleanup)
        self.root=Path(self.temporary.name)
        self.sha="a"*40
        self.pin={"source_sha":self.sha,"app_version":"0.10.0","development":False}
        self.pin_path=self.root/"plxnative-hud-simulator-linux-x86_64.tar.pin.json"
        self.pin_path.write_text(json.dumps(self.pin))
        self.archive=self.root/"plxnative-hud-simulator-linux-x86_64.tar.gz"
        self.archive.write_bytes(b"archive")
        self.assets={self.archive.name:self.archive.read_bytes()}
        self.uploads=[]

    def gh(self,args,**kwargs):
        if args[1]=="api":
            return subprocess.CompletedProcess(args,0,self.sha+"\n","")
        if args[2]=="view":
            return subprocess.CompletedProcess(args,0,json.dumps({"assets":[{"name":name} for name in self.assets]}))
        if args[2]=="download":
            (Path(args[-1])/args[5]).write_bytes(self.assets[args[5]])
        if args[2]=="upload":
            path=Path(args[-1]); self.uploads.append(path.name); self.assets[path.name]=path.read_bytes()
        return subprocess.CompletedProcess(args,0,"","")

    def test_partial_upload_is_completed_and_repeated_attachment_is_idempotent(self):
        with patch.object(hud_release.subprocess,"run",side_effect=self.gh):
            self.assertEqual(hud_release.attach(self.root,self.sha,"v0.10.0"),"v0.10.0")
            self.assertEqual(self.uploads,[self.pin_path.name])
            hud_release.attach(self.root,self.sha,"v0.10.0")
            self.assertEqual(self.uploads,[self.pin_path.name])

    def test_different_existing_bytes_are_never_overwritten(self):
        self.assets[self.archive.name]=b"different"
        with patch.object(hud_release.subprocess,"run",side_effect=self.gh),self.assertRaisesRegex(ValueError,"immutable"):
            hud_release.attach(self.root,self.sha,"v0.10.0")
        self.assertEqual(self.uploads,[])

    def test_stable_release_tag_must_resolve_to_the_exact_pinned_source(self):
        def gh(args,**kwargs):
            if args[1] == "api":
                return subprocess.CompletedProcess(args,0,"b"*40+"\n","")
            return self.gh(args,**kwargs)
        with patch.object(hud_release.subprocess,"run",side_effect=gh),self.assertRaisesRegex(ValueError,"tag.*source"):
            hud_release.attach(self.root,self.sha,"v0.10.0")
        self.assertEqual(self.uploads,[])

    def test_wrong_source_and_development_claims_fail_before_any_github_action(self):
        with patch.object(hud_release.subprocess,"run") as gh:
            for source,tag,development in (("b"*40,"v0.10.0",False),(self.sha,"v0.9.0",False),(self.sha,"",True)):
                with self.assertRaises(ValueError):
                    hud_release.attach(self.root,source,tag,development)
            gh.assert_not_called()

    def test_development_publication_uses_a_source_tag_and_is_always_a_prerelease(self):
        self.pin["development"]=True; self.pin_path.write_text(json.dumps(self.pin))
        calls=[]
        def gh(args,**kwargs):
            calls.append(args)
            if args[2]=="view":
                return subprocess.CompletedProcess(args,1,"","release not found")
            return subprocess.CompletedProcess(args,0,"","")
        with patch.object(hud_release.subprocess,"run",side_effect=gh):
            tag=hud_release.attach(self.root,self.sha,development=True)
        self.assertEqual(tag,"hud-simulator-"+self.sha)
        create=next(args for args in calls if args[2]=="create")
        self.assertIn("--prerelease",create)
        self.assertEqual(create[create.index("--target")+1],self.sha)


if __name__ == "__main__":
    unittest.main()
