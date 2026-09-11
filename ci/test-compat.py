#!/usr/bin/env python3
"""Host-only auxv contract tests, sanitizers, and assertion mutation controls."""
import argparse
import os
from pathlib import Path
import platform
import subprocess

ROOT = Path(__file__).resolve().parent.parent
CASES = ('valid short eintr empty partial unterminated open-error read-error '
         'close-error primary-error late-read-error long bound over-bound concurrent cancel disabled').split()

def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--output', type=Path, default=Path('/tmp/plx-compat-tests'))
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    cc = os.environ.get('CC', 'cc')
    flags = ['-std=c11', '-Wall', '-Wextra', '-Werror', '-pthread', '-g', '-O1']
    source = ROOT / 'ci/test-compat-auxv.c'
    log = (out / 'results.log').open('w')

    def run(command, expected=0):
        result = subprocess.run([str(x) for x in command], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, timeout=90)
        log.write(f'COMMAND {command}\nEXIT {result.returncode}\n{result.stdout}\n')
        log.flush()
        if expected == 'failure':
            assert result.returncode != 0, f'Mutation survived: {command}'
        else:
            assert result.returncode == expected, result.stdout
        return result

    binary = out / 'auxv-test'
    run([cc, *flags, source, '-o', binary])
    for case in CASES:
        run([binary, case])
    print(f'PASS: {len(CASES)} production-parser cases')
    for name, sanitizer in [('asan-ubsan', 'address,undefined'), ('tsan', 'thread')]:
        probe = out / f'probe-{name}.c'
        probe.write_text('int main(void) { return 0; }\n')
        exe = out / f'probe-{name}'
        built = subprocess.run([cc, *flags, f'-fsanitize={sanitizer}', str(probe), '-o', str(exe)],
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        supported = built.returncode == 0
        if supported:
            runtime = subprocess.run([str(exe)], stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, text=True, timeout=30)
            supported = runtime.returncode == 0
        if not supported:
            detail = built.stdout if built.returncode else runtime.stdout
            log.write(f'SKIP {name}: toolchain/runtime unavailable\n{detail}\n')
            print(f'SKIP: {name} runtime unavailable (see log)')
            continue
        binary = out / f'auxv-{name}'
        run([cc, *flags, f'-fsanitize={sanitizer}', source, '-o', binary])
        for case in CASES:
            run([binary, case])
        print(f'PASS: {name}, {len(CASES)} cases')

    original = (ROOT / 'src/compat/getauxval.c').read_text()
    mutations = [
        ('return', 'value = aux_records[i].value;', 'value = 0;', 'valid'),
        ('errno', 'result_errno = incoming;', 'result_errno = 0;', 'valid'),
        ('partial', 'offset += (size_t)n;', 'offset = sizeof(struct aux_record);', 'short'),
    ]
    for name, before, after, case in mutations:
        assert original.count(before) == 1
        mutant = out / f'mutant-{name}.c'
        mutant.write_text(original.replace(before, after))
        binary = out / f'mutant-{name}'
        # The errno mutation leaves incoming unused; suppress only that warning.
        run([cc, *flags, '-Wno-unused-variable', f'-DAUX_SOURCE="{mutant}"', source, '-o', binary])
        run([binary, case], expected='failure')
    print('PASS: return, errno, and partial-read negative controls rejected')
    if platform.system() == 'Linux':
        binary = out / 'auxv-native'
        run([cc, *flags, '-D_GNU_SOURCE', '-DPLX_AUXV_HOST_TEST',
             ROOT / 'src/compat/getauxval.c', ROOT / 'ci/test-compat-native.c', '-o', binary])
        run([binary])
        print('PASS: same-process native Linux comparison')
    else:
        log.write('SKIP native Linux comparison: host is not Linux\n')
        print('SKIP: native Linux comparison (host is not Linux)')
    log.close()

if __name__ == '__main__':
    main()
