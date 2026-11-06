"""Linux loader dependency resolution preserves the app's pinned sibling libraries."""
from pathlib import Path
import hashlib
import io
import json
import shutil
import tempfile
import zipfile
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/"tools"))
import hud_bundle


class Dependencies(unittest.TestCase):
    def test_ldd_receives_only_the_known_input_directories_for_sibling_sonames(self):
        paths = [Path("/build/ffmpeg/lib/libavcodec-plx.so.63"),Path("/build/libass/libass-plx-host.so.0")]
        output = "libavutil-plx.so.61 => /build/ffmpeg/lib/libavutil-plx.so.61 (0x123)\nlibc.so.6 => /lib/libc.so.6 (0x456)\n"
        with patch.dict("os.environ", {"LD_LIBRARY_PATH":"/unrelated/developer-libs"}):
            with patch.object(hud_bundle.subprocess,"run",return_value=subprocess.CompletedProcess([],0,output)) as ldd:
                found = hud_bundle.native_dependencies(paths)
        self.assertEqual(found,{"libavutil-plx.so.61":Path("/build/ffmpeg/lib/libavutil-plx.so.61")})
        for call in ldd.call_args_list:
            self.assertEqual(set(call.kwargs["env"]["LD_LIBRARY_PATH"].split(":")),{"/build/ffmpeg/lib","/build/libass"})

    def test_a_missing_dependency_is_a_hard_failure(self):
        with patch.object(hud_bundle.subprocess,"run",return_value=subprocess.CompletedProcess([],0,"libthing.so => not found")):
            with self.assertRaisesRegex(ValueError,"unresolved"):
                hud_bundle.native_dependencies([Path("/build/app")])


class PinContract(unittest.TestCase):
    def test_pin_preserves_the_source_bound_sdk_protocol_and_profiles(self):
        manifest = {"protocol": 1, "app_version": "0.10.0", "source_sha": "a" * 40,
                    "supported_formats": [{"package": "1.0", "hud": "1.0"}],
                    "schemas": {"manifest": {"1.0": "manifest-id"}, "hud": {"1.0": "hud-id"}},
                    "limits": {"package": {"1.0": {"artifact_bytes": 2097152}}},
                    "development": True, "files": {"bin/plxnative-sim": "b" * 64}}
        pin = hud_bundle.pin_contract(manifest)
        self.assertIs(type(pin["protocol"]), int)
        self.assertEqual(pin["protocol"], 1)
        self.assertEqual(set(pin), set(manifest) - {"files"})
        for field in pin:
            self.assertEqual(pin[field], manifest[field])


class Corpus(unittest.TestCase):
    def test_sdk_carries_both_document_schemas_and_native_archive_corpus(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)/"repo";stage=Path(temporary)/"sdk";root.mkdir();stage.mkdir()
            info={"protocol":1,"supported_formats":[{"package":"1.0","hud":"1.0"}],
                  "schemas":{kind:{"1.0":"https://plxnative.com/hud/schema/"+kind+"/1.0.json"} for kind in ('manifest','hud')},
                  "limits":{"package":{"1.0":{"artifact_bytes":2097152}}}}
            schemas={kind:{"$id":versions['1.0'],"type":"object"} for kind,versions in info['schemas'].items()}
            metadata={"package_format":"1.0","id":"com.example.fixture","version":"1.0.0","name":"Fixture",
                      "author":"Example","license":"CC0-1.0","hud":{"format":"1.0","entry":"hud.json"}}
            for kind,schema in schemas.items():
                path=root/'site/hud/schema'/kind/'1.0.json';path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(schema))
            inputs=[]
            for prefix,name in [('rust-modules/appkit/src/hud_format/fixtures/','minimal'),('site/hud/examples/invalid/','unknown-key')]:
                for member,value in [('manifest.json',metadata),('hud.json',{'layout':{},'unknown':True} if name=='unknown-key' else {'layout':{}})]:
                    path=root/prefix/name/member;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(value));inputs.append(path.relative_to(root).as_posix())
            # This writer is an explicit test double. Production invokes the native Rust packer.
            buffer=io.BytesIO()
            with zipfile.ZipFile(buffer,'w',compression=zipfile.ZIP_STORED) as archive:
                archive.writestr('hud.json','{}');archive.writestr('manifest.json',json.dumps(metadata))
            artifact=buffer.getvalue()
            calls=[]
            def native(binary,*arguments,**kwargs):
                calls.append(arguments)
                if arguments[0]=='--hud-schema': return schemas[arguments[1]]
                self.assertEqual(arguments[0],'--hud-pack')
                Path(arguments[arguments.index('--out')+1]).write_bytes(artifact)
                return {'protocol':1,'findings':[],'bytes':len(artifact),'package_sha256':hashlib.sha256(artifact).hexdigest(),'manifest':metadata}
            with patch.object(hud_bundle,'ROOT',root),patch.object(hud_bundle,'native_json',side_effect=native):
                hud_bundle.prepare_authoring(stage,inputs,Path('/native'),{},info)
            index=json.loads((stage/'corpus/manifest.json').read_text())
            self.assertEqual(index['protocol'],1)
            self.assertEqual(len(index['cases']),7)
            self.assertEqual({doc['kind'] for case in index['cases'] for doc in case['documents']},{'manifest','hud'})
            self.assertEqual(len([case for case in index['cases'] if not case['documents']]),4)
            self.assertTrue(all(case['invalid'] for case in index['cases'] if not case['documents']))
            self.assertEqual(sum(call[0]=='--hud-pack' for call in calls),1)
            self.assertEqual((stage/'corpus/canonical.plxhud').read_bytes(),artifact)
            for kind,schema in schemas.items(): self.assertEqual(json.loads((stage/'schema'/kind/'1.0.json').read_text()),schema)

    def test_an_sdk_without_bundle_support_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary,self.assertRaisesRegex(ValueError,'bundle'):
            hud_bundle.prepare_authoring(Path(temporary),[],Path('/native'),{}, {'protocol':1})


if __name__ == "__main__":
    unittest.main()
