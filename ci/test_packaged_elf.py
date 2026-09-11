#!/usr/bin/env python3
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
spec=importlib.util.spec_from_file_location('packaged',Path(__file__).with_name('check-packaged-elf.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

def package(path, files):
    data=io.BytesIO()
    with tarfile.open(fileobj=data,mode='w:gz') as t:
        for name,blob in files.items():
            info=tarfile.TarInfo('usr/palm/applications/test/'+name);info.size=len(blob);t.addfile(info,io.BytesIO(blob))
    blob=data.getvalue();header=f'{"data.tar.gz":<16}{0:<12}{0:<6}{0:<6}{"100644":<8}{len(blob):<10}`\n'.encode()
    path.write_bytes(b'!<arch>\n'+header+blob+(b'\n' if len(blob)%2 else b''))

class Closure(unittest.TestCase):
    def test_full_closure_and_mutations(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);elf=b'\x7fELFtest';linkmap=b'LOAD own.o';trace=b'own.o'
            for suffix,data in [('.link.map',linkmap),('.link.trace',trace)]: (root/('app'+suffix)).write_bytes(data)
            (root/'app.link.json').write_text(json.dumps(dict(archive_excluded=True,**{k:hashlib.sha256(v).hexdigest() for k,v in [('elf_sha256',elf),('map_sha256',linkmap),('trace_sha256',trace)]})))
            ipk=root/'test.ipk';package(ipk,{'app':elf});m.check(ipk,root)
            for files in [{'app':elf+b'changed'},{'app':elf,'hidden-handler':elf},{}]:
                package(ipk,files)
                with self.assertRaises((ValueError,OSError)):m.check(ipk,root)

if __name__=='__main__': unittest.main()
