#!/usr/bin/env python3
"""Host regression of the production C dispatch gate; no firmware or device access."""
import pathlib
import re
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class NativeLoadGate(unittest.TestCase):
    def test_constructed_object_is_not_dispatchable_until_load_returns(self):
        source = (ROOT / 'src/starfish.c').read_text()
        body = re.search(r'static void \*sf_ready_object\(void\) \{(.*?)\n\}', source, re.S).group(1)
        harness = '''
#include <assert.h>
#include <stddef.h>
typedef struct { char object[16]; } SfSlot;
static SfSlot slot;
static int ready, returned;
#define SMP_READY() ready
#define LOAD_RETURNED() returned
static SfSlot *sf_current_slot(void) { return &slot; }
static void *sf_ready_object(void) { BODY }
int main(void) {
    ready = 1; returned = 0;
    assert(sf_ready_object() == NULL);
    returned = 1;
    assert(sf_ready_object() == slot.object);
    ready = 0;
    assert(sf_ready_object() == NULL);
}
'''.replace('BODY', body)
        with tempfile.TemporaryDirectory() as td:
            src = pathlib.Path(td) / 'gate.c'
            exe = pathlib.Path(td) / 'gate'
            src.write_text(harness)
            subprocess.run(['cc', '-std=c11', str(src), '-o', str(exe)], check=True)
            subprocess.run([str(exe)], check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
