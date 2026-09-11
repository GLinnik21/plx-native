#!/usr/bin/env python3
"""Project-local NDK driver: exclude unlicensed archive and attest each ELF link.

Compilation uses the unchanged SDK. Link evidence stays beside the output and is
validated before success is returned. No SDK file/specs is modified.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    sdk = Path(os.environ.get('WEBOS_SDK', str(Path.home() / 'webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot')))
    real = sdk / 'bin/arm-webos-linux-gnueabi-gcc'
    args = sys.argv[1:]
    linking = '-o' in args and not any(x in args for x in ('-c', '-S', '-E'))
    if not linking:
        return subprocess.call([str(real), *args])
    output = Path(args[args.index('-o') + 1]).absolute()
    mapfile = Path(str(output) + '.link.map')
    tracefile = Path(str(output) + '.link.trace')
    receipt = Path(str(output) + '.link.json')
    for path in (mapfile, tracefile, receipt):
        path.unlink(missing_ok=True)
    command = [str(real), *args, '-tno-glibc-polyfill',
               f'-Wl,-Map={mapfile},--cref,-t']
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    sys.stdout.buffer.write(result.stdout)
    sys.stderr.buffer.write(result.stderr)
    tracefile.write_bytes(result.stdout + result.stderr)
    if result.returncode:
        return result.returncode
    from importlib.util import spec_from_file_location, module_from_spec
    spec = spec_from_file_location('link_evidence', Path(__file__).with_name('check-link-evidence.py'))
    checker = module_from_spec(spec)
    spec.loader.exec_module(checker)
    try:
        checker.check_inputs(mapfile.read_bytes(), tracefile.read_bytes())
        if output.read_bytes()[:4] != b'\x7fELF':
            raise ValueError('link output is not ELF')
    except (ValueError, OSError) as error:
        print(f'arm-cc: evidence rejected: {error}', file=sys.stderr)
        return 1
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    receipt.write_text(json.dumps({'schema': 1, 'elf_sha256': sha(output),
        'map_sha256': sha(mapfile), 'trace_sha256': sha(tracefile),
        'compiler_sha256': sha(real.resolve()), 'archive_excluded': True}, indent=2) + '\n')
    return 0

if __name__ == '__main__':
    sys.exit(main())
