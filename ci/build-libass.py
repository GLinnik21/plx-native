#!/usr/bin/env python3
"""Build the private ASS facade with a pinned, statically contained font stack.

Only plx_ass_* leaves the resulting shared library. No firmware FreeType, FriBidi,
HarfBuzz, libass, or platform font provider can affect its ABI or rendering.
The same source pins and configuration build both the ARM library and host one.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import posixpath
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PINS = ROOT / 'ci/libass-dependencies.json'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args, *, cwd=None, env=None):
    subprocess.run([str(x) for x in args], cwd=cwd, env=env, check=True)


def archive(dep, folder):
    target = folder / dep['archive']
    if target.is_file() and digest(target) == dep['sha256']:
        return target
    # ARM and host builds share downloads, but never a partial pathname.
    fd, candidate = tempfile.mkstemp(prefix=target.name + '.', dir=folder)
    os.close(fd)
    try:
        run(['curl', '-fL', '--retry', '3', '--connect-timeout', '30',
             dep['url'], '-o', candidate])
        if digest(Path(candidate)) != dep['sha256']:
            raise ValueError('source checksum mismatch: ' + dep['archive'])
        os.replace(candidate, target)
    finally:
        Path(candidate).unlink(missing_ok=True)
    return target


def extract(path, folder):
    # Keep Python 3.9 compatibility with the macOS system Python. Reject escaping
    # names/links before extracting the checksum-pinned upstream release.
    with tarfile.open(path) as contents:
        for entry in contents:
            name = posixpath.normpath(entry.name)
            if name.startswith('/') or name == '..' or name.startswith('../'):
                raise ValueError('unsafe source archive path')
            if entry.issym() or entry.islnk():
                name = posixpath.normpath(posixpath.join(posixpath.dirname(name), entry.linkname)
                                         if entry.issym() else entry.linkname)
                if name.startswith('/') or name == '..' or name.startswith('../'):
                    raise ValueError('unsafe source archive link')
            elif not entry.isfile() and not entry.isdir():
                raise ValueError('special source archive member')
        contents.extractall(folder)


def verify_library(path, darwin, readelf=None):
    """Prove the bundled font stack stayed private and needs no C++ runtime."""
    if darwin:
        load_commands = subprocess.check_output(['otool', '-L', str(path)], text=True).splitlines()[1:]
        needed = [line.strip().split(' (', 1)[0] for line in load_commands]
        allowed = {'@loader_path/' + path.name, '/usr/lib/libSystem.B.dylib', '/usr/lib/libiconv.2.dylib'}
        exports = subprocess.check_output(['nm', '-gU', str(path)], text=True).splitlines()
        names = [line.split()[-1].removeprefix('_') for line in exports if line.strip()]
    else:
        dynamic = subprocess.check_output([readelf, '-d', str(path)], text=True)
        needed = re.findall(r'\(NEEDED\).*\[([^\]]+)\]', dynamic)
        allowed = {'libc.so.6', 'libm.so.6', 'libgcc_s.so.1', 'libpthread.so.0', 'ld-linux.so.3'}
        symbols = subprocess.check_output([readelf, '--dyn-syms', '--wide', str(path)], text=True)
        names = []
        for line in symbols.splitlines():
            fields = line.split()
            if len(fields) >= 8 and fields[4] in {'GLOBAL', 'WEAK'} and fields[6] != 'UND':
                names.append(fields[7])
    unexpected = set(needed) - allowed
    if unexpected:
        raise ValueError('unexpected runtime dependencies: ' + ', '.join(sorted(unexpected)))
    if not names or any(not name.startswith('plx_ass_') for name in names):
        raise ValueError('font stack leaked a symbol outside the private ASS facade')
    print('libass: dependency/export isolation PASS', flush=True)


def build():
    host = bool(os.environ.get('HOST'))
    darwin = host and platform.system() == 'Darwin'
    target = 'host' if host else 'arm'
    prefix = ROOT / ('vendor/libass-prefix-host' if host else 'vendor/libass-prefix')
    work = ROOT / ('vendor/libass-build-host' if host else 'vendor/libass-build')
    sources = ROOT / 'vendor/libass-sources'
    for directory in [work, sources, prefix, ROOT / 'pkg']:
        directory.mkdir(parents=True, exist_ok=True)
    # A kernel-held lock disappears with its owner; no stale pid-file recovery.
    # Host and ARM have separate workspaces and can build concurrently.
    with (work / '.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        build_locked(host, darwin, target, prefix, work, sources)


def build_locked(host, darwin, target, prefix, work, sources):
    pins = json.loads(PINS.read_text())
    archives = {d['id']: archive(d, sources) for d in pins}
    env = os.environ.copy()
    # Avoid package auto-detection and caller-supplied flags changing the stack.
    # Our key records the compiler/toolchain; all build flags are owned here.
    for key in ['CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBS',
                'PKG_CONFIG_PATH', 'PKG_CONFIG_SYSROOT_DIR']:
        env.pop(key, None)
    env['PKG_CONFIG_LIBDIR'] = str(prefix / 'lib/pkgconfig')
    cmake = shutil.which('cmake')
    pkgconfig = shutil.which('pkg-config')
    if not cmake or not pkgconfig:
        raise ValueError('cmake and pkg-config are required (see setup-environment)')
    flags = ['-O2', '-fPIC', '-fvisibility=hidden', '-funwind-tables', '-fno-omit-frame-pointer',
             '-ffile-prefix-map=' + str(ROOT) + '=/plxnative']
    cmake_flags = ['-DCMAKE_BUILD_TYPE=Release', '-DBUILD_SHARED_LIBS=OFF',
                   '-DCMAKE_POSITION_INDEPENDENT_CODE=ON', '-DCMAKE_INSTALL_LIBDIR=lib',
                   '-DCMAKE_PREFIX_PATH=' + str(prefix),
                   '-DCMAKE_INSTALL_PREFIX=' + str(prefix)]
    configure_flags = ['--prefix=' + str(prefix), '--libdir=' + str(prefix / 'lib'),
                       '--disable-shared', '--enable-static', '--with-pic']
    if host:
        cc, cxx = shutil.which('cc'), shutil.which('c++')
        ar, ranlib = shutil.which('ar'), shutil.which('ranlib')
        if not all([cc, cxx, ar, ranlib]):
            raise ValueError('host C/C++ compiler, ar and ranlib are required')
        identity = subprocess.check_output([cc, '--version'])
        if darwin:
            flags.append('-mmacosx-version-min=11.0')
            cmake_flags.append('-DCMAKE_OSX_DEPLOYMENT_TARGET=11.0')
    else:
        sdk = Path(os.environ.get('WEBOS_SDK', str(Path.home() / 'webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot'))).resolve()
        cross = sdk / 'bin/arm-webos-linux-gnueabi-'
        cc = str(ROOT / 'ci/arm-cc.py')
        cxx, ar, ranlib = [str(cross) + name for name in ['c++', 'ar', 'ranlib']]
        sysroot = sdk / 'arm-webos-linux-gnueabi/sysroot'
        env['WEBOS_SDK'] = str(sdk)
        if not Path(cxx).is_file():
            raise ValueError('webOS NDK missing; run make setup-env')
        identity = subprocess.check_output([str(cross) + 'gcc', '--version'])
        for path in [Path(str(cross) + 'gcc.br_real'), Path(str(cross) + 'ld'), sysroot / 'lib/libc.so.6']:
            identity += digest(path.resolve()).encode()
        cmake_flags += ['-DCMAKE_SYSTEM_NAME=Linux', '-DCMAKE_SYSTEM_PROCESSOR=arm',
                        '-DCMAKE_SYSROOT=' + str(sysroot),
                        '-DCMAKE_FIND_ROOT_PATH=' + str(prefix) + ';' + str(sysroot),
                        '-DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER',
                        '-DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY',
                        '-DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY',
                        '-DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY',
                        '-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY']
        flags.append('--sysroot=' + str(sysroot))
        configure_flags += ['--host=arm-webos-linux-gnueabi']
    env.update(CC=cc, CXX=cxx, AR=ar, RANLIB=ranlib,
               CFLAGS=shlex.join(flags), CXXFLAGS=shlex.join(flags))
    cmake_flags += ['-DCMAKE_C_COMPILER=' + cc, '-DCMAKE_CXX_COMPILER=' + cxx,
                    '-DCMAKE_AR=' + ar, '-DCMAKE_RANLIB=' + ranlib,
                    '-DCMAKE_C_FLAGS=' + shlex.join(flags),
                    '-DCMAKE_CXX_FLAGS=' + shlex.join(flags)]
    key = hashlib.sha256(PINS.read_bytes() + Path(__file__).read_bytes() + identity +
                         json.dumps([target, platform.machine(), cmake_flags, configure_flags]).encode() +
                         (ROOT / 'ci/arm-cc.py').read_bytes() +
                         (ROOT / 'ci/check-link-evidence.py').read_bytes()).hexdigest()
    stamp = prefix / '.dependencies-key'
    static_names = ['libass.a', 'libharfbuzz.a', 'libfribidi.a', 'libfreetype.a']
    if not stamp.is_file() or stamp.read_text() != key or not all((prefix / 'lib' / n).is_file() for n in static_names):
        for name in ['source', 'objects']:
            shutil.rmtree(work / name, ignore_errors=True)
            (work / name).mkdir()
        # This directory contains only this recipe's derived output.
        shutil.rmtree(prefix)
        prefix.mkdir()
        for dep in pins:
            extract(archives[dep['id']], work / 'source')
        by_id = {d['id']: work / 'source' / (d['id'] + '-' + d['version']) for d in pins}
        jobs = os.environ.get('JOBS', str(min(os.cpu_count() or 4, 8)))

        def cmake_build(name, options):
            directory = work / 'objects' / name
            run([cmake, '-S', by_id[name], '-B', directory, *cmake_flags, *options], env=env)
            run([cmake, '--build', directory, '--parallel', jobs], env=env)
            run([cmake, '--install', directory], env=env)

        def autotools_build(name, options):
            directory = work / 'objects' / name
            directory.mkdir()
            run([by_id[name] / 'configure', *configure_flags, *options], cwd=directory, env=env)
            if name == 'fribidi':
                # Its release contains generated Unicode tables. Build the library
                # alone; CLI/manpage targets need host help2man and cannot run ARM
                # executables during a cross build.
                run(['make', '-C', 'lib', '-j' + jobs], cwd=directory, env=env)
                run(['make', '-C', 'lib', 'install'], cwd=directory, env=env)
                run(['make', 'install-pkgconfigDATA'], cwd=directory, env=env)
            else:
                run(['make', '-j' + jobs], cwd=directory, env=env)
                run(['make', 'install'], cwd=directory, env=env)

        cmake_build('freetype', ['-DFT_DISABLE_' + dep + '=ON'
                               for dep in ['ZLIB', 'BZIP2', 'PNG', 'HARFBUZZ', 'BROTLI']])
        autotools_build('fribidi', [])
        cmake_build('harfbuzz', ['-DHB_HAVE_FREETYPE=ON', '-DFREETYPE_LIBRARY=' + str(prefix / 'lib/libfreetype.a'),
                                '-DFREETYPE_INCLUDE_DIRS=' + str(prefix / 'include/freetype2'),
                                '-DHB_HAVE_CORETEXT=OFF', '-DHB_BUILD_UTILS=OFF',
                                '-DHB_BUILD_SUBSET=OFF', '-DHB_BUILD_RASTER=OFF',
                                '-DHB_BUILD_VECTOR=OFF', '-DHB_BUILD_GPU=OFF'])
        autotools_build('libass', ['--disable-fontconfig', '--disable-coretext', '--disable-directwrite',
                                  '--disable-require-system-font-provider', '--disable-libunibreak',
                                  '--disable-test', '--disable-compare', '--disable-profile'])
        stamp.write_text(key)
    # The facade changes more often than the upstream stack; relinking it does not
    # require a clean FreeType/HarfBuzz rebuild.
    name = 'libass-plx.0.dylib' if darwin else ('libass-plx-host.so.0' if host else 'libass-plx.so.0')
    output = prefix / 'lib' / name
    objects = [prefix / 'lib' / n for n in static_names]
    wrapper_flags = [f for f in flags if f != '-fvisibility=hidden']
    command = [cc, *wrapper_flags, '-I' + str(ROOT / 'include'), '-I' + str(prefix / 'include'),
               str(ROOT / 'src/ass.c'), *map(str, objects), '-lm']
    if darwin:
        symbols = sorted(set(re.findall(r'\b(plx_ass_\w+)\s*\(', (ROOT / 'include/ass.h').read_text())))
        if not symbols:
            raise ValueError('no exported ASS facade functions')
        exports = work / 'exports.txt'
        exports.write_text(''.join('_' + s + '\n' for s in symbols))
        command += ['-dynamiclib', '-liconv', '-Wl,-install_name,@loader_path/' + name,
                    '-Wl,-exported_symbols_list,' + str(exports)]
    else:
        exports = work / 'exports.map'
        exports.write_text('{ global: plx_ass_*; local: *; };\n')
        command += ['-shared', '-Wl,-soname,' + name, '-Wl,--no-undefined',
                    '-Wl,--exclude-libs,ALL', '-Wl,--version-script=' + str(exports)]
    run([*command, '-o', output], env=env)
    verify_library(output, darwin, shutil.which('readelf') if host else str(cross) + 'readelf')
    staged = ROOT / 'pkg' / name
    candidate = staged.with_name(staged.name + '.' + str(os.getpid()) + '.new')
    sidecars = ['.link.map', '.link.trace', '.link.json'] if not host else []
    try:
        shutil.copy2(output, candidate)
        if not host:
            run([str(cross) + 'strip', '--strip-unneeded', candidate], env=env)
            run([sys.executable, ROOT / 'ci/stage-link-evidence.py', output, candidate, '--stripped'], env=env)
        elif darwin:
            run(['codesign', '--force', '--sign', '-', candidate])
        # Publish the target last. A failed strip, signature or receipt must never
        # leave a new-enough Make target that skips the failed recipe next time.
        for suffix in sidecars:
            os.replace(str(candidate) + suffix, str(staged) + suffix)
        os.replace(candidate, staged)
    finally:
        candidate.unlink(missing_ok=True)
        for suffix in sidecars:
            Path(str(candidate) + suffix).unlink(missing_ok=True)
    print('libass: ' + str(staged.relative_to(ROOT)), flush=True)


if __name__ == '__main__':
    try:
        build()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit('build-libass: ' + str(error))
