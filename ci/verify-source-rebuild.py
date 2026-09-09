#!/usr/bin/env python3
"""Rebuild an actual source bundle using isolated compiler, Cargo and dependency caches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from source_bundle import validate

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('archive',type=Path);p.add_argument('--expect-snapshot',required=True)
p.add_argument('--work',type=Path,required=True,help='must not exist')
p.add_argument('--rust-toolchain',default='nightly')
p.add_argument('--private-values',required=True,type=Path)
a=p.parse_args();manifest=validate(a.archive,a.expect_snapshot,[v.encode() for v in json.loads(a.private_values.read_text())])
sdk=Path(os.environ.get('WEBOS_SDK',str(Path.home()/'webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot')))
ndk_hash=hashlib.sha256((sdk/'bin/arm-webos-linux-gnueabi-gcc.br_real').read_bytes()).hexdigest()
if manifest.get('build_environment',{}).get('ndk',{}).get('gcc_sha256') != ndk_hash:
    sys.exit('refusing different or unrecorded NDK GCC identity')
work=a.work.resolve()
if work.exists():sys.exit('refusing to overwrite a rebuild workspace')
work.mkdir(parents=True)
compiler=Path(subprocess.check_output(['rustc','+'+a.rust_toolchain,'--print','sysroot'],text=True).strip())
isolated=work/'compiler'
shutil.copytree(compiler,isolated,ignore=lambda directory,names:['src'] if Path(directory)==compiler/'lib/rustlib' else [])
source=work/'source';env=dict(os.environ,RUSTUP_HOME=str(work/'rustup'),CARGO_HOME=str(work/'cargo'),CARGO_NET_OFFLINE='true',PLX_BUILD_CACHE=str(work/'cache'))
commands=[
 [sys.executable,str(Path(__file__).with_name('restore-source-inputs.py')),str(a.archive.resolve()),'--expect-snapshot',a.expect_snapshot,'--destination',str(source),'--rust-sysroot',str(isolated)],
 ['rustup','toolchain','link','source-rebuild',str(isolated)],
 ['make','-C',str(source),'RUST_NIGHTLY=source-rebuild','RELEASE=1','FLAVOR=stable',
  'PLX_SENTRY_DSN=','PLX_POSTHOG_KEY=','PLX_SENTRY_DSN_DEV=','PLX_POSTHOG_KEY_DEV=','ipk']]
result={'ndk_gcc_sha256':ndk_hash,'source_snapshot_sha256':a.expect_snapshot,'rebuild_status':'FAIL','configuration':'production ARM stable, no private telemetry configuration','bit_for_bit':'NOT_CLAIMED','commands':commands}
with (work/'build.log').open('w') as log:
 for command in commands:
  run=subprocess.run(command,env=env,stdout=log,stderr=subprocess.STDOUT)
  if run.returncode:
   result['exit']=run.returncode;(work/'result.json').write_text(json.dumps(result,indent=2)+'\n');sys.exit(run.returncode)
result['rebuild_status']='PASS';result['artifacts']={x.name:hashlib.sha256(x.read_bytes()).hexdigest() for x in (source/'pkg').glob('*.ipk')}
(work/'result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({'rebuild_status':'PASS','source_snapshot_sha256':a.expect_snapshot,'result':str(work/'result.json')}))
