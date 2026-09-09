#!/usr/bin/env python3
"""Create and verify the versioned source asset before any binary publication."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--inputs',required=True,type=Path)
p.add_argument('--output',required=True,type=Path)
p.add_argument('--rust-toolchain',default='nightly')
p.add_argument('--binary',required=True,type=Path)
p.add_argument('--private-values',required=True,type=Path,help='explicit local confidential-literal list; never archived')
a=p.parse_args();root=Path(__file__).resolve().parents[1]
subprocess.run([sys.executable,str(root/'ci/collect-source-inputs.py'),'--archive-root',str(root),'--output',str(a.inputs),'--rust-toolchain',a.rust_toolchain],check=True)
mapfile=a.inputs/'source-inputs.json';m=json.loads(mapfile.read_text());sdk=Path(os.environ.get('WEBOS_SDK',str(Path.home()/'webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot')))
real=sdk/'bin/arm-webos-linux-gnueabi-gcc.br_real'
m['build_environment']['ndk']={'gcc_sha256':hashlib.sha256(real.read_bytes()).hexdigest(),'archive_policy':'ci/arm-cc.py excludes glibc-polyfills'}
m['build_environment']['configuration']={'RELEASE':'1','FLAVOR':'stable','features':'no-default-features','private_telemetry':'excluded; reconstruction defaults without endpoints'}
mapfile.write_text(json.dumps(m,indent=2)+'\n')
subprocess.run([sys.executable,str(root/'ci/make-source-bundle.py'),'--output',str(a.output),'--dependencies',str(mapfile),'--dependency-root',str(a.inputs),'--binary',a.binary.name+'='+str(a.binary),'--private-values',str(a.private_values)],check=True)
release=a.output.with_name(a.output.name+'.release.json');record=json.loads(release.read_text())
subprocess.run([sys.executable,str(root/'ci/check-source-bundle.py'),str(a.output),'--expect-snapshot',record['source_snapshot_sha256'],'--release-manifest',str(release),'--binary',a.binary.name+'='+str(a.binary),'--private-values',str(a.private_values)],check=True)
print(record['source_snapshot_sha256'])
