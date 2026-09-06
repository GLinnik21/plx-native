# Replay fixtures

A directory here is one RECORDING (restructure spec §5.3): `manifest.json` (the header) and
`rec-NNNN.jsonl` segments, taken on the simulator against `tests/mock_pms.py` by arming
`plxnative-rec`. They are regression assertions pinned to the build lineage that recorded them
(§5.5): `tools/plxnative-rec diff` compares two, `tools/plxnative-rec check` verifies one against
`ALPHABET.json`, and `tests/test_harness.py` verifies every committed one on every `make check`.

Recording one: `make sim`, then `tests/focusfp.sh --rec --only <n>` (the flow's own tokens, the
mock server, `plxnative-rec` armed), `tools/plxnative-rec check` and `import <dir> <n>-<name>`, and
`tests/focusfp.sh --replay --only <n>` to read the app's own `replay: done … verdict=` line. Phase
2 hashes the press machine, the route/overlay words and the focus fingerprint; the stores still
fetch live, so a landing arriving on a different frame is the expected divergence and is reported.

**Nothing here may carry a household byte.** The mock server's names are `s[0-9a-f]{8}`; every
other string a recording holds is a protocol constant named in `ALPHABET.json`. A recording taken
against a REAL server lives in `plxnative-recordings/` (gitignored, refused by the outbound guard) and
never comes here. A fixture whose `manifest.json` carries `"anchor": true` refuses `--rebaseline`.
