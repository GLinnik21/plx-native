#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('restore',Path(__file__).with_name('restore-source-inputs.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

def crate(root,folder,name,version,checksum):
    p=root/folder;p.mkdir(parents=True)
    (p/'Cargo.toml').write_text(f'[package]\nname = "{name}"\nversion = "{version}"\n')
    (p/'.cargo-checksum.json').write_text(json.dumps({'package':checksum,'files':{}}))
    return p

class RuntimeVendor(unittest.TestCase):
    def test_build_std_dependency_is_available_without_registry(self):
        with tempfile.TemporaryDirectory() as d:
            r=Path(d)/'runtime';a=Path(d)/'app';r.mkdir();a.mkdir()
            crate(r,'hashbrown-1','hashbrown','1.0.0','123')
            m.merge_runtime_vendor(r,a)
            self.assertTrue((a/'rust-runtime-hashbrown-1/Cargo.toml').is_file())
    def test_identical_registry_package_is_not_duplicated(self):
        with tempfile.TemporaryDirectory() as d:
            r=Path(d)/'runtime';a=Path(d)/'app';r.mkdir();a.mkdir()
            crate(r,'libc-1','libc','1.0.0','123');crate(a,'libc','libc','1.0.0','123')
            m.merge_runtime_vendor(r,a);self.assertEqual(len(list(a.iterdir())),1)
    def test_conflicting_source_identity_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            r=Path(d)/'runtime';a=Path(d)/'app';r.mkdir();a.mkdir()
            crate(r,'libc-1','libc','1.0.0','123');crate(a,'libc','libc','1.0.0','456')
            with self.assertRaises(ValueError):m.merge_runtime_vendor(r,a)

if __name__=='__main__':unittest.main()
