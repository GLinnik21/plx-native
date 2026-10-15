#!/usr/bin/env python3
"""`make deploy` must ship exactly what `make ipk` stages, minus the entries that get their own
handling for a documented reason (the binary + crash handler's `.new`+`mv` dance, the bundled
FFmpeg libraries' own retirement loop, and the LAB session file's ship-or-remove rule).

This runs `make -s print-app-files` / `print-deploy-files` — two query targets that only echo a
Makefile variable, exactly the pattern `docs/agent-reference.md` prescribes instead of `make -p`
for asking the Makefile a question. Neither touches the television or needs `.tv-host`, so it
needs no TV lock and no device.

It exists because the two lists drifted silently before: `deploy` used to scp the binary, the
crash handler, the appinfo and the fonts by name, one line per file, while `ipk` staged whatever
`APP_FILES` said — so `pkg/splash.png`, both icon sizes, `pkg/OFL.txt` and
`THIRD-PARTY-NOTICES.md` were in every `.ipk` and in NO deployed app directory, and nothing here
noticed for weeks. Now that `deploy`'s plain-file list (`DEPLOY_FILES`) is defined as `APP_FILES`
minus exactly those three carve-outs, this test is really asking "does the Makefile still define
it that way" — which is exactly the kind of drift a future edit could reintroduce by hand.
"""
from __future__ import annotations

import pathlib
import subprocess
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent


def make_print(target: str, flavor: str = "debug") -> list[str]:
    out = subprocess.run(
        ["make", "-s", target, f"FLAVOR={flavor}"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    return out.split()


class DeployManifest(unittest.TestCase):
    def test_deploy_files_is_app_files_minus_the_three_carve_outs(self):
        for flavor in ("debug", "stable"):
            with self.subTest(flavor=flavor):
                app_files = make_print("print-app-files", flavor)
                deploy_files = make_print("print-deploy-files", flavor)
                handler_bin = make_print("print-sentry-handler", flavor)
                ffmpeg_libs = make_print("print-ffmpeg-staged", flavor)
                carve_outs = {"pkg/plxnative", *handler_bin, *ffmpeg_libs}
                # LAB_FILES is empty unless LAB=1 is passed, and this test never sets it — so the
                # session file never appears in either list here, and is asserted separately below.
                expected = [f for f in app_files if f not in carve_outs]
                self.assertEqual(deploy_files, expected)

    def test_every_deploy_file_basename_is_unique(self):
        # `deploy` scp's the whole list to one directory in one connection; a basename collision
        # would silently make one file overwrite another the way `ipk`'s own `cp` would too.
        deploy_files = make_print("print-deploy-files")
        names = [pathlib.Path(f).name for f in deploy_files]
        self.assertEqual(len(names), len(set(names)), names)

    def test_splash_and_notices_are_in_the_deployed_set(self):
        # The reported bug, pinned by name: these four were in APP_FILES/the .ipk and never
        # reached a deployed app directory.
        deploy_files = make_print("print-deploy-files")
        names = {pathlib.Path(f).name for f in deploy_files}
        for must in ("splash.png", "icon.png", "OFL.txt", "THIRD-PARTY-NOTICES.md", "le-roots.pem"):
            with self.subTest(must=must):
                self.assertIn(must, names)

    def test_the_subtitle_faces_ship_in_their_own_directory_not_the_flat_list(self):
        # `APP_FILES` is staged with a flat `cp`; the subtitle faces are read from fonts/subs/ of the
        # app directory (`player::ass::fonts`), so they travel in `SUBS_FONT_FILES` and every recipe
        # that handles the flat list handles this one too (ipk, deploy, verify-deploy).
        subs = make_print("print-subs-font-files")
        self.assertEqual(
            sorted(subs),
            sorted([*(f"pkg/fonts/subs/{p.name}" for p in (ROOT / "pkg/fonts/subs").glob("*.ttf")),
                    "pkg/fonts/subs/manifest.json"]))
        flat = set(make_print("print-app-files")) | set(make_print("print-deploy-files"))
        self.assertFalse(flat & set(subs), "a flat copy would drop the directory")
        names = [pathlib.Path(f).name for f in subs]
        self.assertEqual(len(names), len(set(names)), "verify-deploy keys by basename")
        self.assertFalse(set(names) & {pathlib.Path(f).name for f in flat})
        manifest = __import__("json").loads((ROOT / "pkg/fonts/subs/manifest.json").read_text())
        self.assertEqual(sorted(f["file"] for f in manifest["faces"]), sorted(n for n in names if n.endswith(".ttf")))
        makefile = (ROOT / "Makefile").read_text()
        for recipe, needle in (("ipk", "cp $(SUBS_FONT_FILES) $(STAGE)/$(SUBS_FONT_DIR)/"),
                               ("deploy", "$(SCP) $(SUBS_FONT_FILES) tv:$(APPDIR)/$(SUBS_FONT_DIR)/"),
                               ("verify-deploy", "$(SUBS_FONT_FILES) $(if $(LAB),pkg/lab.json,)")):
            with self.subTest(recipe=recipe):
                self.assertIn(needle, makefile)

    def test_lab_session_file_is_never_in_the_plain_list(self):
        # It has its own ship-or-remove rule (present under LAB=1, actively deleted otherwise),
        # which a plain unconditional scp must never duplicate.
        for lab in (None, "1"):
            args = ["make", "-s", "print-deploy-files", "FLAVOR=debug"]
            if lab:
                args.append(f"LAB={lab}")
            out = subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout
            self.assertNotIn("lab.json", out)


if __name__ == "__main__":
    unittest.main()
