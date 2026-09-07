# Replay fixtures

A directory here is one RECORDING (restructure spec §5.3): `manifest.json` (the header) and
`rec-NNNN.jsonl` segments, taken on the simulator against `tests/mock_pms.py` by arming
`plxnative-rec`. They are regression assertions pinned to the build lineage that recorded them
(§5.5): `tools/plxnative-rec diff` compares two, `tools/plxnative-rec check` verifies one against
`ALPHABET.json`, and `tests/test_harness.py` verifies every committed one on every `make check`.

Recording one: `make sim`, then `tests/focusfp.sh --rec --only <n>` (the flow's own tokens, the
mock server, `plxnative-rec` armed), `tools/plxnative-rec check` and `import <dir> <n>-<name>`, and
`tests/focusfp.sh --replay --only <n>` to read the app's own `replay: done … verdict=` line. The
hash is `AppFrame{route,overlay,focus,tree}`: the press machine, the route/overlay words and the
focus fingerprint from phase 2, plus — since phase 5b — `tree`, which is `Dispatcher::state_hash`
(every live container instance's `LogicalState`, the tree's shape and surface phases, the engine's
focus and the queue depth). The stores still fetch live, so a landing arriving on a different frame
is the expected divergence and is reported.

There are two committed fixtures today, both **anchors** — `1-boot-home-chip-grid` (re-recorded at
5b's new `state_fp` pin) and, new in 5b, `6-settings-family` (the Settings family and first-run
Favourites, the pages that answer `FocusSource::Engine`/`HitSource::Engine`) — take the count from
this directory's own listing, not from here. An anchor refuses `--rebaseline` (below); when a
change instead bumps the recorded state SHAPE (`schema` or `state_fp` — 5b's `tree:u64` term did
exactly this), the old fixture cannot even be LOADED, so `tools/plxnative-rec rerecord <dir> <name>`
is the verb: it verifies the shape actually moved and replaces the fixture, anchor flag preserved.

**Nothing here may carry a household byte.** The mock server's names are `s[0-9a-f]{8}`; every
other string a recording holds is a protocol constant named in `ALPHABET.json`. A recording taken
against a REAL server lives in `plxnative-recordings/` (gitignored, refused by the outbound guard) and
never comes here. A fixture whose `manifest.json` carries `"anchor": true` refuses `--rebaseline`
(a behaviour-change replacement, evidenced by a divergence record); `rerecord`, above, is the
shape-bump escape an anchor does not refuse.
