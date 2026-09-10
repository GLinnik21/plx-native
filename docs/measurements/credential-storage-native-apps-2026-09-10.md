# How Apple TV, Prime Video, Netflix and YouTube protect the stored sign-in at rest — 2026-09-10

Issue: whether PlxNative's own storage-at-rest choice (keymanager3 where it round-trips, a 0600
plaintext fallback everywhere else — `rust-modules/src/plex/session.rs`'s "Storage model" doc) is
reasonable against what the commercial native apps on the same television actually do. Answered by
decompiling the four apps' own binaries and reading the device, not by inferring from a vendor SDK
name. This is the **device** companion to the storage-file hardening landed the same day
(`repair_owned_mode`, the 0777 spool finding below).

Dev set: LG 49SM9000PLA, webOS 4.10.2, board m16p3. Read-only device reads (`ls`/`stat`, 32-byte
file heads). Harvested binaries per-app (Apple TV, Prime Video, Netflix, YouTube/Cobalt),
decompiled with the project's usual `decompile-tv-lib` workflow.

## Verdict

All three commercial streaming apps that store a real credential (Apple TV, Prime Video, Netflix)
encrypt it and root the key in the SoC secure store through `libdile_crypto.so.0`
(`SESTORE_*` / `HAL_SSTR_MakeSecureData`). Each partner has its **own named** DILE entry point
(`DILE_CRYPTO_APPTV_ReadPrivKey`, `DILE_CRYPTO_ReadAmazonSecret`, `DILE_CRYPTO_NF_ReadAppData`). The
**precise** claim about those entry points, not the looser "homebrew cannot call them" this doc used
to make: each is **partner-scoped by name** — calling `DILE_CRYPTO_ReadAmazonSecret` from outside
Prime Video would be reading (or clobbering) *Amazon's* key slot, not opening a vendor-neutral one —
and the generic `COMMON_Read*` calls this binary also exports are read-only platform keys, not a
write path. **No vendor-neutral write entry point was found in `libdile_crypto.so.0`.** That is an
absence-of-evidence statement from reading the exported symbol table, not a permission/ACG check
actually performed against this app's own LS2/DILE access — nobody here tried calling
`DILE_CRYPTO_ReadAmazonSecret` from a PlxNative binary and recorded the refusal code, so "PlxNative
literally cannot reach it" remains inferred, not measured. None of the three uses a key-manager LS2
service the way PlxNative's `keymanager3` probe does, and none keeps its key beside the data or as a
constant in the binary.

Exceptions, and they matter: Netflix's own token file is **plaintext JSON, mode 0600, in a mode
0700 directory** — the tokens are useless on their own, since playback needs a device-sealed MSL
session key the TEE holds. YouTube/Cobalt stores its cookie jar in the clear, mode 0600, protected
by nothing else — which is exactly where PlxNative already was before this branch's hardening.

## Per app

| app | uid | credential location | encrypted | key source | mode |
|---|---|---|---|---|---|
| Apple TV `com.apple.appletv` | 5281 | db8 kind `com.apple.database.keyvaluetable:1` (LevelDB in `/var/db/main`, root:root); nothing in the app's own directory | AES per value, AES key RSA-wrapped into db8 filestorage `aes.bin` | RSA keypair in LG_SecureStorage via `DILE_CRYPTO_APPTV_{Read,Write}PrivKey` (called from the app's own `liblma.so`) | store is unreachable by any app uid at all; the db8 caller permission is scoped to `com.apple.*` |
| Prime Video `amazon` | 6870 | `plugins/com.amazon.ignition.framework.storage/var/data/buckets/secure/storage` (144 bytes ciphertext) | yes | `DILE_CRYPTO_{Read,Write}AmazonSecret`, called from `libprime-video-device-layer.so` (`LinuxDeviceLayerSecureStorageBackendContext::write`, offset `0x3767c`) | **0644** in a 0775 directory — the mode is not relied on, since the bytes are already ciphertext |
| Netflix `netflix` | 5639 | `diskstore/msl-backup-tokens/...` plaintext JSON; `secure-application-data/ramshadow/*` opaque | tokens **no**; the session keys underneath them **yes**, sealed via `IDeviceCryptoContext::contextExportSealedKey` (the TEE `TeeCryptoAdapter`) | TEE/TA; a DILE `NF_*` half exists in the binary but was not traced end to end | 0600, in 0700 directories |
| YouTube `youtube.leanback.v4` (Cobalt) | 6030 | `content/.starboard.<base64>.storage` — an `SAV1` protobuf savegame holding plaintext cookies | **no** | none | 0600, in a 0775 directory |

Caveat, does not change the finding: a 20-character constant sits in Apple TV's `.rodata` right
after the `LgSecureStorage Instantiate` string; its role (a PEM passphrase, a salt) was not
determined.

## The chain, per app — what is proven and what is not

Each row above collapses "what is stored → where encryption is called → where the key comes from →
what jail/mode protects the file" into one line each; spelled out separately, because the four links
are not equally solid and a reader skimming the table can otherwise credit a stronger claim than was
actually measured:

- **Apple TV.** Stored: per-value AES ciphertext in a root-owned LevelDB, outside the app's own
  directory entirely. Encryption called from: the app's own `liblma.so`, observed calling
  `DILE_CRYPTO_APPTV_{Read,Write}PrivKey`. Key source: an RSA keypair inside `LG_SecureStorage`,
  read only through that named entry point. Jail/mode: the db8 store itself is unreachable by any
  app uid — this is the one link in the whole survey backed by a caller-permission check
  (`com.apple.*`) rather than by mode alone. All four links traced end to end; nothing here is
  inferred.
- **Prime Video.** Stored: 144 bytes of ciphertext under the app's own plugin data directory.
  Encryption called from: `libprime-video-device-layer.so`'s
  `LinuxDeviceLayerSecureStorageBackendContext::write`, calling `DILE_CRYPTO_WriteAmazonSecret`
  directly. Key source: the same named DILE call, key material not further traced (the SoC secure
  store's own internals were out of scope for a device read). Jail/mode: 0644 in a 0775 directory —
  deliberately not relied on, since the bytes are already ciphertext. Three of four links traced;
  the key's own storage inside the SoC secure store is asserted by the DILE call's existence, not
  independently confirmed.
- **Netflix.** Stored: plaintext JSON tokens in `diskstore/msl-backup-tokens/`, uselessly on their
  own. Encryption called from: **not traced end to end** — a `DILE_CRYPTO_NF_*` half exists in the
  binary, referenced near `IDeviceCryptoContext::contextExportSealedKey`, but which call site in
  Netflix's own code actually invokes it, and for which of the MSL session keys, was not walked.
  Key source: asserted to be the TEE/TA (`TeeCryptoAdapter`'s own naming), not independently proven
  by tracing a call into the TEE boundary itself — this repo has no way to inspect what runs inside
  the TEE. Jail/mode: 0600 in 0700 directories, for the plaintext tokens that are useless without the
  TEE-held key. **The weakest-proven link in this survey**: everything downstream of "a `TeeCryptoAdapter`
  symbol exists" is inference from naming, not from a traced call.
- **YouTube/Cobalt.** Stored: plaintext cookies inside an `SAV1` protobuf savegame. Encryption
  called from: nothing — no DILE/TEE call was found anywhere near the storage read. Key source: none.
  Jail/mode: 0600 in a 0775 directory. **"No encryption" is well-proven** (an absence is easier to
  show than a hidden call), but **"the plaintext cookie is sufficient on its own to authenticate as
  the account" was never independently confirmed** — Cobalt/YouTube's own session model could still
  require a second, unobserved factor (a device-bound cookie attribute, a server-side IP/device
  check) that this device read cannot see from the file alone. Treat the YouTube row's "no
  encryption" as proven and its practical exploitability as unverified.

## The jail is the real difference, not the crypto choice

A **retail** app runs under `/etc/jail_native.conf`: `mountappdir` puts only that app's own
directory in the mount namespace, `homedir $APPDIR`. A **Developer Mode** app — PlxNative and every
homebrew app included — runs under `/etc/jail_native_devmode.conf`: no `mountappdir` at all,
`/media/developer` mounted read-write, **whole**, measured `drwxrwxrwx` root:root. Every app has its
own uid; all of them share gid 5000. So under Developer Mode, **mode is the entire boundary** a file
in `/media/developer` can draw against a sibling app — and even a 0600 file (unreadable by a peer)
can still be `unlink`ed or `rename`d by a peer that has write access to the shared parent directory,
which is a denial/substitution primitive, not a disclosure one; PlxNative's `write_atomic`
(`O_NOFOLLOW` + `create_new`) and `read_owned_regular` (ownership + regular-file check on the open
fd) are what turn a substituted name into "rejected," never "parsed as ours."

Measured on the set, the same session this task's hardening was scoped from:

```
drwxrwxrwx 0    0    /media/developer
-rw------- 6910 5000 2780 com.beb.plxnative-auth.json
-rw------- 6085 5000 4399 com.beb.plxnative.debug-auth.json
-rwxrwxrwx 5862 5000 1104 com.glin.plexpoc-auth.json                  (retired install — pre-rename app id)
-rwxrwxrwx 6085 5000 2226 com.beb.plxnative.debug-telemetry-spool.bin (0777)
-rwxrwxrwx 6085 5000  162 com.beb.plxnative.debug-telemetry.json      (0777)
```

Both `com.beb.plxnative-auth.json` and `com.beb.plxnative.debug-auth.json` are exactly the mode
`write_atomic` has always produced (0600); the two `.debug-telemetry-*` files and the retired
`com.glin.plexpoc-auth.json` were found at 0777. **Git archaeology (full history of
`telemetry/spool.rs`, `telemetry/mod.rs`/`consent.rs`, and `paths.rs`, from the very first telemetry
commit through this branch's HEAD) found no commit that ever wrote either telemetry file, or the
session file, at anything but 0600** — every creation path (`write_atomic`'s temp file, and the one
now-removed direct `OpenOptions::new().append(true).create(true).mode(0o600)` the spool used before
2026-09-02's hardening) named `0o600` explicitly. The spool's own first-append-of-every-boot
compaction path additionally rewrites the file fresh via `write_atomic` whenever this process's
in-memory record count is unknown — which is every process start — so a widened mode found at boot
would ordinarily be overwritten with a fresh 0600 file the very next append **unless the widening
happened after that first append already ran within the same boot**, or the app had not appended
since. **The 0777 finding is not derivable from this repository's own history as something the app
wrote** — the most consistent explanation is an external actor in the shared `/media/developer`
namespace (a peer devmode app's own install/deploy tooling, or a manual `chmod` during an earlier
debugging session), consistent with the retired, long-dead `com.glin.plexpoc` install carrying the
identical 0777 mode on its own session file despite that binary also having written the file 0600 at
creation. Regardless of cause, the pre-existing code's response to finding a widened mode was to
**refuse every further write to it, forever, silently** (`telemetry/spool.rs`'s
`"telemetry: refused an unsafe spool file"`) — which is what made this a silent, permanent telemetry
outage on the affected install rather than a one-time cosmetic finding. This branch's fix
(`session::repair_owned_mode`) repairs an owned regular file's mode via `fchmod` on the already-open
fd instead of refusing it, on every read and append of every such file, logged once per repair.

**A follow-up review (same day) made a correct point this first fix did not yet cover: repairing the
mode does not make the CONTENT trustworthy.** If the widening ever carried a group/other WRITE bit
(any of `0o022`), another uid could have rewritten the bytes between our last write and this read —
a "yes" in a rewritten consent file is not this person's decision, a token in a rewritten session
file is not provably this account's, a record in a rewritten spool is not provably ours. Read-only
widening (`0o044`/`0o055`) stays a disclosure problem only, and that content is still trusted. The
`trust/widened-files` branch adds that distinction: `repair_owned_mode` now returns what it found
(`ModeTrust::Trusted` / `Repaired { readable_only }`), the session and consent loaders discard a
write-widened file's content outright (no session / both consent categories unanswered) rather than
parse it, every marker/probe file is ignored and deleted rather than trusted, and the spool
truncates instead of appending onto a write-widened copy.

## Comparison

| against | PlxNative |
|---|---|
| Apple TV | weaker on every axis — they have an SoC-held key plus a root-owned store; `keymanager3` matches only the key half, and only where it is available and proven |
| Prime Video | weaker on the bytes (cleartext vs. SoC-sealed ciphertext), stronger on the mode (0600 vs. their measured 0644) |
| Netflix | equal mechanism where PlxNative is unsealed (0600 cleartext), weaker consequence (PlxNative's plaintext token is directly usable against the server; Netflix's plaintext token is not usable without the TEE-sealed session key beside it) |
| YouTube | equal, marginally stronger (PlxNative's ownership check on every read/append, and atomic `O_NOFOLLOW` writes; Cobalt's storage read was not observed to check either) |

## Recommendation (owner decision, via this decompile)

Keep `keymanager3` as the sealed state: it is the only mechanism available to a homebrew app that
reproduces a platform-held key, even though it is not proven present or trustworthy on every
firmware the way the three commercial apps' SoC/TEE paths are (each ships with its own signed
partner entitlement; PlxNative has none). One file path, two honest states: sealed where keymanager3
is *proven* to round-trip across a launch, 0600 cleartext everywhere else — never redesigned into a
third state. A per-install random key stored beside the data **does not protect against the
realistic threat here**: a reader of one 0600 file (the same uid, or an fchmod/root actor) can read
the other 0600 file sitting right beside it just as easily, and it **does not replace a
platform-held key** — it is still an ordinary file this app's own process reads at startup, not a
secret rooted in hardware or an OS-held store. It is not therefore worthless: it **does separate the
key from the data against a PARTIAL leak** — a copy of the data file alone (a backup, a single file
pulled off `/media/developer` by something that can read but not enumerate every file) is not
directly usable without the second file, which a wholesale directory read defeats but a narrower one
does not. A serial- or LGUDID-derived key is worse than even that: every process on the box can read
those inputs without needing this app's uid at all. The effort this finding actually earns: the two 0777 telemetry files (fixed
this branch — `repair_owned_mode`), an explicit statement of the Developer Mode shared-namespace
exposure in `SECURITY.md`, and confirming the existing ownership/regular-file checks reach every
owned file rather than only the session file (also this branch — `crashreport.rs`'s crash-mark
reader was the one remaining owned file using a bare `std::fs::read`).

Encryption at rest, however it is keyed, protects against: a pulled flash image, a
`/media/developer` copied over ssh, a backup, or a peer app that can read files but cannot execute
code as this app's uid. It does not protect against root, or against code already running as this
app's own uid — and neither does Apple TV's SoC-backed scheme, for the same reason: the key
unwrap happens in-process, under that app's own uid, the same as ours does.
