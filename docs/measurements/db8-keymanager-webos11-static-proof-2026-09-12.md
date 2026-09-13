# DB8 and Keymanager3 on webOS 11.2 — static proof ledger

Date: 12 September 2026. Scope: the exact `43.21.71.01` firmware image (Rockhopper
`11.2.0-3701`, `HE_DTV_W23O_AFABATAA`). This is interoperability evidence from binaries owned by
the maintainer. It records behavior in paraphrase and contains no recovered vendor source.

This ledger distinguishes three things which must not be conflated:

1. exact firmware code proves that a request shape reaches a particular implementation path;
2. injected tests prove how PlxNative handles replies, failures and lost replies from that path;
3. only a run on matching hardware can prove the installed role, OPTEE health, quota, persistence
   across power loss, and LG Content Store acceptance.

## Binary identity

| component | SHA-256 |
|---|---|
| `libmojodb.so.3.2.0` | `e7f698871d1efd9be2a8017e2fdde62228349fd456f1e9db962d0b6a5c25b02e` |
| `keymanager3` | `475c10720e86def25b524bf25c0dfba590a458b32efa3142a0ca8dfb2f7ad5c2` |

Ghidra addresses below use image base `0x10000`; they are not raw ELF offsets. The local working
evidence is under `/private/tmp/plx-db8-final-*.log`, `/private/tmp/plx-keymanager-final-*.log` and
`/private/tmp/plx-ls2-final-cancel.log`. Those files are intentionally not distributed.

## DB8 assumptions

| PlxNative assumption | Exact 11.2 evidence | Verdict / limit |
|---|---|---|
| Fixed `_id` plus exact `_rev` is compare-and-swap | `MojDb::putImpl` at `0x44d20` retrieves an existing ID and compares both halves of its signed 64-bit revision. A stale revision returns `-3961`; omitting revision for a live object returns `-3960`. The revision-mismatch string at `0x7b45c` reaches the comparison path at `0x44fac`. | Proven for the ordinary put flags used by the helper. |
| An absent-to-create race cannot force-overwrite a winner | The same put path changes to update semantics when the fixed ID already exists and then requires its revision. The helper sends neither force nor merge. | Proven for the current request. |
| `_rev` remains an integer | `MojDbObjectHeader::visit` at `0x5f8d0` emits the revision through the integer-property visitor from an `int64` field. | Proven; the helper rejects non-positive/non-exact revisions. |
| `get` returns `_id`, `_kind`, `_rev` | The header visitor emits those root fields and conditionally `_del`; `handleGet` at `0x68b48` traverses requested IDs into results. | Proven. |
| A JSON string is opaque to DB8 | Header processing applies to the outer object; value serialization treats `state` as a string. The old 4.10.2 runtime reproduction separately showed why nested JSON must not be stored directly. | Proven representation; maximum accepted string/frame size is not device-proven. |
| Private-kind owner is the helper service | `MojDbKind::hasOwnerPermission` at `0x56724` performs exact caller-domain/owner comparison unless the caller is DB admin; `checkOwnerPermission` at `0x56804` denies mismatch. | Proven. Repeating `putKind` remains owner-scoped. |
| DB8 is persistent outside the app jail | `maindb.conf` selects `/var/db/main`, a 10 MiB wildcard-owner quota and synchronous storage; commit paths reach the configured backend. | Persistent service path proven; sudden-power-loss durability and the configured maximum object size remain runtime gates. |
| Normal uninstall removes private owner data | `removePrivateDataByOwner` at `0x46f18` is reached by `handleRemoveAppData` at `0x6838c`; installer evidence passes both application and service owners. | Normal installer path proven; Dev Mode expiry/channel switching and Store behavior remain unproven. |

## Keymanager3 assumptions

| PlxNative assumption | Exact 11.2 evidence | Verdict / limit |
|---|---|---|
| The packaged caller can request crypto operations | Public groups include `securitykey.operation`; the API map contains generate, remove, begin, finish and abort. Bulk removal uses the separate management group. | Capability is present; the installed role and live OPTEE initialization can still fail. |
| `generateKey` accepts AES-256/GCM | `Handler::generateKey` `0x21b60` reaches `generateKeyInternal` `0x217b4` and `AESHelper::checkGenerateParamValidation` `0x2c7f4`; supported sizes include 256 and purpose/padding are arrays. | Proven parser path. |
| Omitted GCM tag sizes receive compatible defaults | `AddGenerateEssentialParam` `0x2c3f8` inserts `min_mac_length=128`; `AddOperateEssentialParam` `0x2c1a8` defaults `mac_length=128`. | Proven from resolved literals and reachable code. |
| Encrypt/decrypt with `padding:["None"]` is valid | `checkOperateParamValidation` `0x2cc2c` accepts the purpose/mode shape and rejects PKCS7 for GCM/CTR. | Proven. |
| `begin` returns the IV and a numeric handle represented as text | `beginInternal` `0x25f90` obtains the operation, `setIvIfNeeded` `0x2f0c8` extracts/base64-encodes IV, and the reply converts its `uint64` handle to decimal text. | Proven; malformed/missing fields remain helper failures. |
| `finish` consumes and produces base64 | `Handler::finish` `0x226c0` validates the string handle, decodes optional string inputs and base64-encodes non-empty output. | Proven. Missing output is invalid for PlxNative's non-empty auth payload. |
| Keys are bound to authenticated caller identity | `getAppId` `0x2a934` reads message identity; `getKeyId` `0x2a984` prefixes the logical name with it; `begin` `0x20e58` resolves that ID; `setClientParam` `0x2f05c` also supplies `app_id`. | Binding mechanism proven. The helper's actual identity still depends on installed LS2 roles at runtime. |
| Successful generation stores a key blob | The completion path from `generateKeyInternal` reaches the service's DB8 `putKey`; the OPTEE adaptor produces the protected blob. | Reachable path proven, live round-trip still a device gate. |
| Removing a key is irreversible hardware erasure | OPTEE `removeKey` `0x31ec8` and `removeAppKeys` `0x31f18` return success without hardware revocation; the effective removal is deletion of the DB8 key blob at `DB8Helper::removeKey` `0x28a08`. | Stronger claim contradicted. Do not claim protection against a root-level restoration of old DB8 blobs. |
| `abort` releases a known failed operation | `Handler::abort` `0x205f4` validates the string handle and reaches OPTEE abort `0x31e04`. | Proven best-effort path. A lost `begin` reply provides no handle to abort. |
| A two-second timeout proves no side effect occurred | `LSCallCancel` `0x20500` removes local call tracking and attempts transport cancellation; it does not roll back remote Keymanager or DB8 work. | False. Every timeout is an uncertain-outcome case; keys possibly referenced by a late success must be retained and DB8 writes reconciled by operation ID/digest. |

## Installation contexts

The native and Dev Mode jails expose the LS2 socket and writable `/tmp`; Dev Mode additionally
exposes developer storage. The common installer path can install the bundled native helper and its
role/permission files. A root-installed package launched through the ordinary jailed application
path uses the same architecture. Starting the helper arbitrarily as root is not supported because
it changes the authenticated owner. Seller Lounge validation and an LG Content Store install have
not been observed and remain acceptance gates.

## Required injected traces

The helper tests must model, without real credentials:

- unavailable, timeout, numeric service refusal and malformed replies at generate/begin/finish;
- wrong plaintext and post-write readback failure, followed by ACL repair success, conflict,
  rejection, lost reply and late success;
- remote Keymanager success after local timeout, including a lost begin handle;
- DB8 commit with lost reply, stale `-3961`, missing-revision `-3960`, concurrent readback and
  reconcile/replay after restart;
- a strict routine refresh over healthy ciphertext using persisted pending-auth staging: restart
  before/after each CAS and lost replies must leave old auth active until authenticated promotion;
- untrusted reply fields containing secret bait, which must never cross the closed diagnostic
  schema;
- consent deferred/No/Yes and real development-Sentry arrival with closed cause fields, build,
  flavor, firmware and verified/preservation outcome.

Passing those traces proves PlxNative's reaction to the firmware behavior above. It does not turn
static firmware evidence into a claim that newer hardware, OPTEE, power-loss recovery or Store
distribution was exercised.

## Development Sentry delivery proof

A dev-trigger build on the webOS 4.10.2 television sent seven fixed, credential-free scenarios
through the real consent gate, spool, device TLS stack and development Sentry ingest. The trigger
constructor has no access to Session, LS2 or Keymanager; it uses the same typed producer and
serializer as runtime failures. Sentry created seven distinct issues because the fingerprint
includes failure stage, operation, category and `synthetic` source:

| dev issue | synthetic cause | distinguishing outcome |
|---|---|---|
| `PLX-NATIVE-DEV-B` | seal / generate / unavailable | fresh-login ACL fallback, DB8 verified |
| `PLX-NATIVE-DEV-C` | readback / roundtrip / invalid response | post-write ACL repair, DB8 verified |
| `PLX-NATIVE-DEV-D` | seal / finish / invalid response | fresh-login ACL fallback |
| `PLX-NATIVE-DEV-E` | seal / generate / numeric service refusal | legacy-import ACL fallback, service code `-42` |
| `PLX-NATIVE-DEV-F` | seal / begin / timeout | fresh-login ACL fallback |
| `PLX-NATIVE-DEV-G` | strict seal / generate / unavailable | prior protection unchanged, no DB8 stage |
| `PLX-NATIVE-DEV-H` | strict pending readback / begin / timeout | prior protection unchanged, pending DB8 stage verified |

The first real ingest exposed a platform-side schema problem that local JSON tests could not:
Sentry's sensitive-data filter replaced a field named `auth_preservation` with `[Filtered]`. The
field was renamed to `prior_protection_outcome`; both strict scenarios were resent and the API
returned `unchanged` without filtering. Each fetched event contained `diagnostic_source=synthetic`,
helper protocol 1, the DB8 verification verdict, firmware/build/flavor and hardware contexts, and
the expected closed cause fields. None of the storage contexts contained `token`, `payload`,
`ciphertext`, `key_name`, `errorText` or `path` fields.

After the proof, the synthetic trigger was removed and the television was returned to the
release-feature debug binary. Its next cold process restored the stored account, PMS credential,
server and profile route without QR. The public canary excludes `devtriggers`; these injected
events are development evidence, not behavior reachable in its shipped bytes.
