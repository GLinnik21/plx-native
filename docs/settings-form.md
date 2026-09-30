# Declarative forms and one Settings navigation path

Design record for the five-PR migration that makes every Settings page (and the other action
menus) a declared, ordered list of rows, and every Settings drill-down one stack push. Reviewed
in three rounds by an independent model reviewer before implementation; this file is the
agreed result.

**Status:** PR 1 (`ui/form.rs`) and PR 2 (keyed focus: `TablePart::keys`, `family::form_focus`; the Settings
root on `root_form` / `FormTable`) and PR 3 (one navigation path: `family::form_activate`, `SettingsPage::Picker`, `preferences::PickerPage`; the
Playback / Audio & Subtitles field list on a `FormTable`) and PR 4 (Language, Legal index, Consent controls and the Item / Account / More menus on a
`FormTable`; `FormTable::set_or_open` / `set_sliding` for the menus and the Consent toggles) have landed; PR 5 is open. The track menu and source list still use index focus.

## Goals (owner)

1. "Settings are a straightforward structure. It should be described like SwiftUI, and switching
   the structure should be as easy as switching fields in code." — reordering a page is moving a
   line; no parallel vector, no test that counts rows.
2. "One architecture of Settings navigation, so there are no bugs such as a drill-down that does
   not push." — every drill-down is a family-stack push; no page owns a private submenu.
3. Animations are not lost: push/pop spring, focus springs, appear/dismiss, scrims — all as today.
4. Leaf pages' layout does not change (the pixels of Privacy, Legal, documents, About, Language,
   Playback, Audio & Subtitles, the pickers stay as they are); only root-like structure moves.

## Today (why)

- `screens/settings.rs` `root_sections(&RootInputs) -> (Vec<Section>, Vec<Action>)` pushes rows
  and a parallel `actions` vector by hand; `RootPage::activate` indexes `self.rows[row]`.
- Focus identity is the raw table index everywhere: `family::table_focus` (family.rs:218) sets
  `table.sel = elem`; `TablePart`'s `Focusable` impl (ui/table_screen.rs:146-190) maps elem <->
  index; `RootState.sel` is canon-hashed (settings.rs:979); the family's `remembered` pop seats
  are canon-hashed u32 keys (settings.rs:529). A reorder moves focus keys, fingerprints, replay.
- Two drill-down mechanisms: (a) the family stack (`Fx::Nav(NavOp::Push(SettingsPage::X))`,
  settings.rs ~1278); (b) `PreferencesPage`'s private in-page picker: its own `picker_table`,
  `submenu: RoutePush`, `leaving`, `state.picker: Option<Field>` (preferences.rs:70-134, :481,
  :563), bypassing the stack's back handling, remembered seat and canon.
- The same parallel-vector pattern: `preferences.rs field_section`, `ui/track_menu.rs`,
  `ui/source_list.rs`, `screens/item_menu.rs`, `screens/account_menu.rs`, `ui/more_menu.rs`,
  `screens/legal.rs`, `screens/consent.rs`.

## Model (`rust-modules/src/ui/form.rs`)

- `RowKey(u32)` — a row's stable focus key (NOT `FocusKey`, which already means
  `{entry, elem}` in ui/machine.rs:245). Hand-assigned per page by `Id::key()`; never enum
  discriminant/layout/hash. The `< BAND` check lives in the screens layer (ui/ cannot name
  `screens::registry::BAND`); ui/form.rs takes the ceiling as a parameter or const generic-free
  argument and debug-asserts against it.
- `RowKind<Dest>`: `Nav(Dest)`, `Toggle`, `Choice`, `Button`. Presentation (chevron, toggle
  knob, checkmark) stays on `Row` as today; the kind decides activation.
- `Item<Id, A, Dest> { id: Id, key: RowKey, kind: RowKind<Dest>, action: A, row: Row }`;
  inert slots (separator, note) consume a layout index and have no id/key/action; never
  focusable.
- `Form<Id, A, Dest>`: sections with `.visible(bool)`; `.item(..)`, `.item_if(cond, ..)`,
  `.separator()`, `.note(..)`. Built by a PURE fn from plain inputs (no globals, no closures).
- `FormTable<Id, A, Dest> { table: TableView, bindings: Vec<Option<Binding>> }`. ONE call
  `set(form, keep: Option<&Id>)` replaces table sections AND bindings together so a lookup can
  never read an older rebuild's bindings. Lookups: `id_at(index)`, `binding_at(index)`,
  `index_of(&Id)`, `index_of_key(RowKey)`, `key_at(index)`. Linear scan; no HashMap. `Id:
  PartialEq + Clone`; actions cloned on dispatch. debug-assert no duplicate Id and no duplicate
  key per set().
- Disappearing id on `set` (sign-out removes rows, a server vanishes): nearest surviving
  selectable row in the OLD order — prefer the next, else the previous; menus keep their existing
  safe opening row; `open_sections`' destructive-row rule (table.rs:638) preserved.
- `dim`/busy is visual only; callers keep their busy guards.
- Dynamic rows (per-server plaintext switches): identity = the server's machine id
  (`ServerMachineId(String)` newtype — `MachineId` already names a UI machine,
  ui/machine.rs:555); focus key = a fixed per-page base + position within the dynamic section.
  Deterministic from replayed inputs, so no interner and nothing to restore. Selection restore
  across rebuilds uses the Id (machine id), not the key.
- Cost: one Vec per rebuild; picker lists can exceed 100 rows (language), lookup stays linear;
  covered by an operation-count unit test (not a timed one).

## Focus

- A form-aware table part: `TablePart` gains an optional key map from the `FormTable`; when
  present, ALL FIVE focus hooks — `group_of`, `neighbour`, `place`, `reconcile`, `seat` — and the
  pointer stops (table_screen.rs:194) translate RowKey <-> index. Tables not yet on a
  `FormTable` keep today's index mapping until they migrate, so each PR gates alone.
- `family::table_focus` becomes key-aware for form pages. `RootState.sel` -> the selected
  RowKey; canon writes the key explicitly; `remembered` stores keys. State shape changes are
  deliberate: re-record the affected replay/focusfp anchors in the same PR and say which.

## Navigation

- ONE shared `form_activate(&FormTable, index, fx) -> Option<A>` used by every Settings page:
  a `Nav(dest)` item emits `Fx::Nav(NavOp::Push(dest))` itself and returns None; other kinds
  return the action for the page's `match`. ui/form.rs is generic over `Dest`, so `SettingsPage`
  never enters ui/ (ui/CLAUDE.md layer rule). The family (`RouteSurface::forward`/`request`,
  settings.rs ~252-330) stays the only stack executor.
- Pickers become stack pages: `SettingsPage::Picker(PickerKind)` for Quality, DirectPlay,
  AudioLanguage, SubtitleMode, SubtitleLanguage, ForcedSubtitles; built from a Form of `Choice`
  rows exactly like the Language page. The picker page OWNS its transaction: reads the account
  snapshot, runs the Force confirmation, submits the write (same PMS/session paths + ticket),
  shows busy/retry, pops on a durable receipt; picking the already-checked value pops with no
  write; stale-request recovery as today. Crumb = parent kind title, title = field title (as
  preferences.rs:125 today). Added to the mounter, page ids/words, canon encoding and the recorded
  shape (family.rs:89, settings.rs:433).
- Parent refresh: a picker's durable write bumps a preferences revision (plex/account/
  preferences.rs) that the parent Playback / Audio & Subtitles page observes and reloads on;
  `Enter` alone only does the initial load today (preferences.rs:452,463). Test: after BACK the
  parent readout shows the committed value.
- `PreferencesPage`'s `picker_table`, `submenu`, `leaving`, `state.picker` are DELETED. No page
  in the family owns a `RoutePush` except the family itself (structural test/grep gate).

## Tests that define done

- Unit (ui/form.rs): id/key resolution, inert slots never focusable, duplicate-id/key asserts,
  disappearing-id fallback (next, else previous), op-count over 200 rows.
- Settings root: reorder test — two forms differing only in order produce the same Id-addressed
  behaviour; navigation tests address rows by Id, never by numeric index.
- Structural nav test over every Settings form under every input combination: each Nav item,
  driven by OK, RIGHT and a pointer click through the dispatcher, pushes exactly its dest; BACK
  re-seats focus on the same Id.
- Text-fit matrix (settings_text_fit_tests.rs) calls the real form builders.
- Picker: checked value -> no write + pop; write -> durable -> pop -> parent shows new value;
  retry and Force confirmation preserved.

## Verification tiers

- Every PR: `make check` (report the `test result:` lines), `CARGO_INCREMENTAL=0 cargo +nightly
  check --manifest-path rust-modules/Cargo.toml --lib --no-default-features`, ARM `make`.
- Sim (`ui-sim`): captures of each touched page before/after; leaf pages pixel-identical.
- PR3 (push path changes): which-tier animation checks + TV push/pop verification, and the
  Settings-only FPS suite on the TV (`tests/run.py --fps` over the settings scenes: settings-root,
  settings-privacy, settings-home, settings-legal, settings-idle, legal-document, decision-alert,
  modal-ramp, plus a new picker scene).

## PR sequence

1. `ui/form.rs` — Form, FormTable, RowKey, RowKind, unit tests. No callers yet.
2. Keyed focus (form-aware TablePart, key-aware table_focus, canon keys) + Settings root on
   `root_form` + reorder test; anchors re-recorded.
3. One navigation path: `form_activate` on every Settings page; `SettingsPage::Picker`; delete
   PreferencesPage's private submenu; preferences revision refresh; structural nav test; FPS.
4. Language, Legal index, Consent controls, Item / Account / More menus on FormTable.
5. Track menu and Source list (note/separator slots); delete every remaining parallel
   action/target vector.
