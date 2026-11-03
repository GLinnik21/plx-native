"""Linux loader dependency resolution preserves the app's pinned sibling libraries."""
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
