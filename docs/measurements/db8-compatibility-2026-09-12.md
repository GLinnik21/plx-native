# DB8 compatibility evidence — 2026-09-12

This note records the evidence behind the proposed PlxNative persistence backend. It separates
behavior observed on a television from behavior established by firmware inspection and from
distribution-policy questions that firmware cannot answer.

## Proposed boundary

The native app does not own a DB8 kind directly. A packaged native service with a stable LS2 name
owns private DB8 kinds. The app activates that service through LS2, then exchanges a small,
versioned persistence protocol over an app-private Unix socket. The service exposes no credential
methods on LS2. Both socket peers validate `SO_PEERCRED`; the socket is created with mode `0600`.

DB8 is for the session and compact settings, not image or media caches. The DB8 object contains
only its fixed metadata and an opaque canonical JSON string. This is a correctness boundary, not
cosmetic encoding: the old television was observed adding a reserved `_id` to an object inside a
nested array when the canonical state was stored as a flattened DB8 object. The inspected image
configures a 10 MiB wildcard DB8-owner quota; effective object/frame capacity remains a runtime
gate.

## Compatibility matrix

| Installation/runtime | Result | Evidence strength |
| --- | --- | --- |
| Old webOS, Developer Mode package | Observed working on the tested webOS 4.10.2 Developer Mode TV | End-to-end device evidence from that television |
| Rooted TV, package installed through Homebrew/dev installer | Observed working on the tested rooted development TV via its normal jailed launch path | Root was used for administration, while app and service both ran as the same non-root jail UID |
| Root-started or otherwise unjailed binary | Not a supported persistence identity | Deliberately outside the design; root can bypass the app's security boundary |
| webOS 11.2.0-3701, Developer Mode | Expected to work | Required DB8, installer, LS2 and shared-jail mechanisms are present in an exact affected-reporter firmware; runtime execution on that set is not yet observed |
| webOS 11.2.0-3701, Store runtime | Expected to work if LG installs the packaged service | Exact firmware has the native Store jail, shared `/tmp`, DB8 public API group and service-file generator |
| LG Store submission | Unknown | Firmware proves runtime capability, not Seller Lounge acceptance of a third-party bundled native service |
| Exact 50NU800B6LC / K25LPN report | Unknown at runtime | No public firmware image or exact OTA identifier was available; do not substitute the inspected G3 firmware for this model |

## Old-TV runtime proof

The test used a separate package and identities:

- app: `com.beb.plxnative.db8probe`
- service and private kind owner: `com.beb.plxnative.db8probe.storage`
- package update: `0.0.4` to `0.0.5`

Observed results:

1. A direct anonymous main-process DB8 request reached DB8 but was rejected with `-3963`,
   `permission denied`. A process registered under the stable service name could create, write,
   find and delete its private kind.
2. A normally installed package dynamically activated its native service. The app and service ran
   as UID 6456 inside the same jail. The socket was `0600`, and both peer-credential checks passed.
3. `putKind`, `put` and `find` succeeded. After both processes exited and the socket inode was
   removed, fresh app and service processes read the fixed test nonce successfully.
4. The normal package-service update from `0.0.4` to `0.0.5` reported `update:true`. Fresh
   processes then read the object created by `0.0.4`.
5. `delKind` returned success. The diagnostic package, generated LS2 files and test artifacts were
   removed, and stable PlxNative was restored.

The production debug service was subsequently exercised with the full typed backend. Its first
flattened canonical record reproduced an old-DB8 behavior the nonce probe could not expose: DB8
added a six-character `_id` to `operations[0]`, making strict canonical decoding fail. The helper
then migrated that record through a narrow compatibility reader and rewrote it as
`{_id,_kind,_rev,state}` with `state` an opaque string. A fresh app process reopened the stored
account/server and reached Home without QR. The observed inner state was revision 15 with Session
and Consent migrations complete, auth present, and no nested DB8 IDs. Settings changes produced
additional successful commits. This is process-restart evidence on webOS 4.10.2; no television
power cycle was performed.

Sanitized result excerpts captured from the television:

```text
APP action=write result=PASS response={"returnValue":true,"probe":"PASS","detail":"nonce_matches"}
SOCKET listen=PASS mode=0600
SOCKET_AUTH result=PASS peerUid=6456 helperUid=6456
DB8 stage=1 returnValue=true
DB8 stage=2 returnValue=true
DB8 stage=4 returnValue=true
DB8 stage=3 returnValue=true

# after both processes had exited and the socket had been removed
APP action=read result=PASS response={"returnValue":true,"probe":"PASS","detail":"nonce_matches"}
DB8 stage=3 returnValue=true

# installer output during 0.0.4 -> 0.0.5
state="installed" update=true

# fresh processes after the update
APP action=read result=PASS response={"returnValue":true,"probe":"PASS","detail":"nonce_matches"}

APP action=clear result=PASS response={"returnValue":true,"probe":"PASS","detail":"kind_deleted"}
DB8 stage=5 returnValue=true
```

The two packages used identical binaries and service metadata; only version metadata differed:

```text
0.0.4 IPK  e41bafea0a999ac0017053441320f7a8050ae6c2d09c862031242f7794c88214
0.0.5 IPK  753d107789de139bacdad8f1dfb0f85032de9fa13d9436a56354e1addbc12a73
binary     3ba1de8cb12e877628bbccd426a0a8f723946a7b25bed81f231f0bb155b74c53
```

No television power cycle was performed. Process replacement and an IPK update are proven;
reboot recovery remains a separate device gate.

## Homebrew and Store installation evidence

Homebrew Channel documents installation through `com.webos.appInstallService/dev/install`. On the
rooted development TV tested here, a package installed and launched through the normal path ran its
app and service as the jail UID, not root.

The rooted development television also supplied a Store-side control. Its installed Netflix
package declares a native bundled service (`netflix.service`, executable `NFService`). The normal
Store LS2 directory contains installer-generated manifest, dynamic service, role, client-permission
and API-permission files for it. This shows that the inspected Netflix Store package has generated
LS2 artifacts for a bundled native service; it does not establish runtime activation or
third-party Store acceptance. Netflix is privileged LG-distributed software, so it does not prove
that Seller Review will accept the same package shape from PlxNative.

### Apple TV control: DB8 is transport, not the vault

The installed Apple TV 16.2.3 Store image was copied read-only and inspected without reading its
live DB8 values. Its Squashfs image has SHA-256
`867a53dd9d9881d4aded57ce685ee30eb720e8542d60facf64f90e7559b81407`; the ARM executable carries
build ID `f2682bfb67be60f117d31b26b4c6f14d56238244` and identifies its application build as
16.2.4.32L76.

Apple TV does use DB8 for both ordinary state and credentials, but it does not treat DB8 itself as
secret storage:

- `com.apple.database.keyvaluetable:1` stores indexed `key` plus `value` fields. The native
  `LgSecureStorage::set` path encrypts the value with AES-128-CBC, base64-encodes the ciphertext,
  then inserts or updates that DB8 object.
- `com.apple.database.filestorage:1` stores `filename`, `filedata` and `lastmodified`. The secure
  storage startup path encrypts its AES key and IV with an RSA public key and persists the result
  as `aes.bin` through this DB8-backed file abstraction. Its RSA pair comes from Apple-specific
  fixed `app_pub_key` / `app_priv_key` slots reached through DILE, sestore, the LG security engine
  and a TrustZone ioctl path. The private-key bytes are returned to the Apple process; this is not
  a non-exportable key-handle design.
- The JavaScript storage adapter routes all application local storage through this encrypted
  native layer. Observed keys include accessibility and playback preferences, campaign cookies,
  a device GUID and `jwt`.
- `LibraryAPI` loads and refreshes the `jwt` key through that adapter. Playback code reads the same
  value and emits it as `Authorization: Bearer <jwt>`, proving that this is an authentication
  credential rather than a coincidentally named setting.

Apple TV declares no bundled helper service. It is an `internalInstallationOnly`, trusted,
privileged Store app whose generated LS2 role gives its main executable a stable app identity.
This does not remove the helper requirement demonstrated by PlxNative's Developer Mode package,
whose anonymous main-process DB8 access was denied. It does validate the layered design:
DB8 supplies persistent, identity-gated records, while a separate key mechanism supplies
confidentiality.

The Apple slots are not a reusable third-party API. Exact old-TV `libsestore` checks the process
path against compiled Apple package prefixes before allowing those fixed names. Its generic
permission checker returns success without a per-record owner decision, and generic delete is a
success-returning stub. A normal, unprivileged Developer Mode probe also received the `se` group
and could open all three sestore IPC objects read-write. It performed no store read or write: a
test record could not be guaranteed removable. Generic sestore is therefore rejected as a
PlxNative key backend on this firmware.

Runtime tracing corroborated the static Apple path. A fresh Apple process opened the DILE,
`libhal_sstr` and sestore libraries, `/dev/lg/se0`, and the sestore shared-memory objects. The trace
was restricted to fixed paths and syscall results; no memory, I/O payload, DB value or account
identifier was collected.

### Netflix and native YouTube controls

The Store controls do not use one universal LG credential recipe:

- Netflix's native `NFService` owns DB8 kind `netflix.service.det:1`. Its static call path is
  `AuthTokenHandler::saveTokenTrigger` to `DETData::saveToken` and `DETData::saveToDB`. The token
  path reaches Netflix-specific `DILE_CRYPTO_NF_*` encryption wrappers and the DB8 save path. The
  binary also names the operation "Encrypt token data"; the exact ciphertext handoff across the
  final wrapper boundary is strongly inferred rather than dynamically observed. This is another
  example of DB8 as persistent transport plus a separate partner crypto boundary, not a generally
  available PlxNative API.
- Runtime tracing observed Netflix opening sestore, `diskstore`, and
  `secure-application-data` categories. Category conversion occurred inside the tracer before
  output; raw dynamic filenames and payloads were never emitted. These opens do not identify the
  content or encryption of any particular file.
- YouTube on the tested TV is a native Cobalt application, not a web-app control. Static call
  paths persist cookies and local storage through Cobalt's SQLite/savegame abstraction. No DILE,
  sestore, or keymanager call was found on that user-state path. Runtime tracing observed storage
  reads and mode-0600 create/truncate writes; cookie-path attempts returned `ENOENT`. Whether the
  lower savegame backend encrypts those bytes remains unknown, so neither plaintext nor encrypted
  credential storage is claimed.

These results leave `keymanager3` as the only documented, non-partner-specific confidentiality
primitive found for newer third-party applications. The exact webOS 11 image exposes its normal
crypto operations through the public `securitykey.operation` group. No equivalent general secure
key store with deletion and per-app isolation was established on the old webOS 4 television.

Principal control-binary identities:

```text
Netflix NFService  SHA-256 4cee3acbdb6cbd995c803c832711526b7e12a4ae3de77466da184b9fadd717a2
                   build-id 7f893f483a063fb82c92b4391c21b24f3b292f84
Netflix libnetflix SHA-256 c17f0bb36d17d82bf61b60ecb9e238352d8e14ea5cc737a493de96b4f9f383ec
                   build-id 0c1dd370d9b17d90e192a8a51df472ab45dbde26
YouTube Cobalt     SHA-256 e6baa1db27a63550d231fec817e47585d3611c34eccedfafcad327ac14ab4f6b
                   build-id 32017ff9212a2d647d12ad7a38fb1b3de394f88e
```

## Exact newer firmware evidence

LG firmware 43.21.71 for the OLED G3/C3 family was downloaded from LG support and extracted. An
issue #76 reporter supplied this exact version for an OLED55G3LA. The image identifies itself as:

```text
Rockhopper release 11.2.0-3701 (queue-qilian)
firmware 43.21.71.01
OTA HE_DTV_W23O_AFABATAA
```

Archive hashes:

```text
LG ZIP  f85e3661fc05a9376ceb9ae6f68f2b4e0c31ee8914526186326a3f5239a04d4d
EPK     1fbc7638893293480481d7bbb73994c9ce306046dc5f43feeebab8cf8ca0c602
```

Inspected binary hashes:

```text
appinstalld                         c5110d5e8b14343a21dd0efec8e454edd5bc2ff05baf884d49133c211dd61d1d
mojodb-luna                         2547f6033dcfd426ed6209c43a21e7766b5a5ad94abb9027914c74fc72515ccb
libluna-service2.so.3.21.2          914842781ed4f07f9c5343bfdae66192c722d9f6f6f58c62f15c30bf5fa51d6f
```

Inspection established:

- `mojodb-luna` uses `/var/db/main`; its configured wildcard owner quota is 10 MiB.
- `database.operation` is a public LS2 group and maps to the required DB8 CRUD/kind methods.
- the Developer Mode and Store native jails both mount `/tmp` read-write and expose LS2 runtime
  state; the Developer Mode jail also mounts `/media/developer` read-write.
- appinstalld's common service installation routine emits role, service, client-permission and
  API-permission files for packaged services.
- normal removal passes the app ID and packaged service IDs to DB cleanup as owners. DB8's private
  kind contract also specifies removal on uninstall.

This is static proof of the required mechanism on a real affected-reporter firmware, not runtime
proof on the OLED55G3LA or the separate K25LPN report.

## Release gates and security limits

- Perform a write, full process exit, reboot, read and package-update read on a newer reporter TV.
- Verify rejection of a client from another app UID at both the socket and DB8 boundaries.
- Create the socket in a validated private directory and handle stale sockets, competing helpers,
  helper death and app/helper protocol-version overlap.
- A durable write is acknowledged only after DB8 confirms it. A read error must not mean "no
  account".
- Migrate trusted legacy state by writing and reading back the new record before cleanup. A logout
  tombstone must prevent fallback or downgrade from resurrecting old credentials.
- Keep stable and debug service owners separate and stable across upgrades.
- Do not claim encryption at rest, protection from root, or executable integrity in Developer
  Mode. On the inspected new firmware, Developer Mode boot deliberately makes
  `/media/developer` world-writable.
- Treat DB8 as transport and ACL, never as encryption. On newer firmware, seal credential records
  through capability-probed `keymanager3` under the helper's stable identity. On old firmware,
  private DB8 improves isolation and durability but no general hardware-backed confidentiality
  mechanism has been established; do not disguise a locally stored software key as equivalent.
- Do not bind Apple `DILE_CRYPTO_APPTV_*`, Netflix `DILE_CRYPTO_NF_*`, or generic sestore. The first
  two are partner-specific, while the measured generic API lacks a usable authorization/deletion
  contract.
- Normal uninstall is destructive for private kinds. Developer Mode expiry cleanup and preservation
  across app-ID or distribution-channel changes remain unproven.

## Public references

- [LG webOS TV Database API](https://webostv.developer.lge.com/develop/references/database)
- [LG Keymanager3 API](https://webostv.developer.lge.com/develop/references/keymanager3)
- [LG `services.json` reference](https://webostv.developer.lge.com/develop/references/services-json)
- [webOS OSE DB8 request-domain implementation](https://github.com/webosose/db8/blob/7b551709f5bbd932e7119752e4929014f4bf8837/src/db/MojDbServiceHandlerBase.cpp)
- [Homebrew Channel installation path](https://github.com/webosbrew/webos-homebrew-channel#manual)
- [PlxNative issue #76](https://github.com/GLinnik21/plx-native/issues/76)
- [LG support page carrying firmware 43.21.71 for the inspected family](https://www.lg.com/ae/support/product/lg-OLED48C36LA.AMRG)
