# Drill-in sub-menus in the player track-menu popover

Design record for the multi-page Subtitles / Audio popover (Other languages, per-language pages,
Style pickers) and the table, form and clip primitives it stands on. Planned with an independent
model reviewer over several rounds; this file keeps the decisions and their reasons.

**Status:** PR 1 (table groundwork), the track menu on keyed forms (Settings form migration 5/5,
#339: `ui::track_menu::TrackRow`) and PR 2 (the page stack in the Subtitles tab on that form: Style,
the Size / Position / Color pickers, nav keys, persistence, locks, the rebuild signature and replay
state) and PR 3 (Other languages, the language pages, the image-subtitle badge) are what this
repository has; PRs 4-5 (animation, More -> Quality) are open. The replay anchors were re-recorded for PR 2 because the overlay's state shape changed on
purpose.

## Behaviour

- **Subtitles root:** a Subtitles section (Off plus single-track languages), one section per
  multi-track language of `yours`, an "Other languages N" row, then Timing (a value row, no
  chevron; it hands off to the timing capsule) and Style. **Other languages:** one row per
  language A-Z; a single-track language is a direct pick row, a multi-track one is a "French, 3
  tracks" drill-in. **Language page:** Full / SDH / Forced / Commentary ranked, identical ones
  numbered. **Style:** Size, Position and Color drill into picker pages with checkmarks.
- **Keys:** UP/DOWN move; OK or RIGHT on a Nav row pushes; on a sub-page LEFT/BACK pops and focus
  returns to the opener by semantic id; on the root LEFT/RIGHT switch tabs as before (RIGHT on a
  Nav row pushes instead); BACK on the root dismisses; clicking the "< TITLE" band pops. A track
  pick at any depth commits and dismisses; a Style pick commits and stays. The menu reopens at the
  root.
- **Format badge (owner edit, 2026-10-01):** only image subtitles (PGS/VobSub) show one, a chip with the format's short name ("PGS", "VOBSUB", "DVB" for Plex's `hdmv_pgs_subtitle`, `dvd_subtitle`, `dvb_subtitle`; `sub_layout::image_codec_badge`) that FOLLOWS the row's kind chip (SDH, Forced, External) instead of being outranked by it, so an SDH bitmap track still says what it is; text subtitles show no format, and the design board's "SRT" sub-lines are gone. A kind chip that only repeats the row's own label ("SDH" on the row called SDH) is still dropped; the format never is.
- **Other languages row:** one Nav row on the root reading the language count (no "N languages" header accessory); when the active track lives behind it (a live change put it there) it reads a check and the track's language instead. On the page, a multi-track language that holds the active track reads a check and that variant ("SDH"), otherwise its track count.
- **Page identity:** a language page is `TrackPage::Language(stream)`, its opener `TrackRow::OpenLang(LangId)`, where `stream` is the Plex stream id of the language's first track in the item's FULL list (`sub_layout::LangId`), never a list position: a track leaving mid-play shifts every index, and an index-keyed page would silently relabel to another language. `LangId` also carries a slot, the language's ordinal on the Other languages page, which only decides the row's focus key (re-derived on each build, so the key may move when a language arrives above it; equality ignores the slot). The page's data is `TrackMenuState::other`, refreshed from the same `sub_model` the root is built from; a page whose language is no longer listed (its first track's stream is gone) pops to the root.
- **A language changing shape (one track <-> several):** on the Other languages page a single-track language is a direct pick row and a multi-track one a drill-in, so a language that grows or shrinks while the viewer is on its row gets the focus carried to its new row (`other_row_for`). A language page whose language drops to ONE track stays open and lists that track (a page lists whatever its language still offers; popping is the only shape change), and the pop lands on the language's now-direct row on Other languages.
- **Harness `track:N`:** the N-th track in page order: the root's track rows, then the tracks behind Other languages A-Z, a multi-track language expanded into its ranked page. It commits by the track's own list index (`commit_sub_track`), not by focusing a row, so it reaches a track whose row is on a page that is not showing.
- **Title band:** a larger gap under the "< TITLE" caption (`table::TITLE_GAP`) before its hairline.
- **Why the root row shows the language and the variant is shown on the page:** when the active track lives behind Other languages, the root's row reads a check and that track's LANGUAGE ("French"), not the variant: a stacked variant on a row would read as a description of the row, and the variant ("SDH") is already answered one level down, where the language's drill-in on the page reads the check and the variant and its own page checks the track. An active track is behind that row in two ways. Normally its language is "yours" and it sits on the root, so the row reads the language count. But a CODELESS subtitle (no language code, grouped under "Unknown") and a track whose code names no language `lang_key` recognises are never added to "yours" (`overlay::subtitle_yours_langs` only adds codes `lang_key` accepts), so such a track opens behind Other languages even though it is the active one, and a live change can put any active track there too. In every such case the menu opens, and the live poll falls back, with focus on the Other languages row (the root's landing id is `TrackRow::OpenOther`), never on Off. `yours` stays the opening snapshot so groups do not reshuffle under the viewer.

## Availability and locks

- Style follows Timing's availability: omitted during an ordinary server-side burn (there is no
  client caption to style), dim with the existing locked note only for the app's OWN burn.
- Per active renderer: image -> Size and Position dim and inert ("Image subtitles keep their own
  size and position."); native ASS/SSA -> the same ("Styled subtitles keep ..."); Color stays live in
  both, since the subtitle ink tints bitmaps and ASS.
- A dim row is focusable so the viewer can read why, and it is INERT at the form layer
  (`FormTable::activate` answers `None` for OK and RIGHT alike), not by a guard in each caller.

## Persistence and live preview

One optimistic operation, no deferred publication: `route::select_subtitle_size/position(v)` on the
main thread stores the live atomic, invalidates, and submits a persist-only closure that writes the
session store and never touches the atomic (mirrors `player::set_subtitle_tone`). Settings'
Size/Position arms use the same call, so no completion can overwrite a newer live pick; durable
writes are FIFO on the worker, last submitted wins. A failed write is logged, never alerted over
video, and claims nothing about the next boot. The live value survives BACK and dismissal (global
state, not panel state).

## Focus identity

Each page declares rows with semantic ids, one alphabet for every page: `ui::track_menu::TrackRow`
(`Audio(i)`, `Boost`, `Loudness`, `SubOff`, `Sub(i)`, `Timing`, `Style`, `OpenField(field)`,
`Choice(field, rung)`, `OpenOther`, `OpenLang(lang)`). Selection and saved openers
are stored by id. `RowKey` is a hand-assigned family base plus a STABLE ordinal (the track's index
in the item's list, never a list position): `0x0001_0000` Audio, `0x0002_0000` the DSP pair,
`0x0003_0000` Off, `0x0004_0000` tracks, `0x0005_0000` Timing/Style, `0x0006_0000` the Style
page's drill-ins, `0x0007_0000` picker rungs (one 256-wide block per field), `0x0008_0000` the Other languages
drill-in and `0x0009_0000 +` a language's drill-in, which uses its slot on the Other page instead of
a track index. `OpenLang` carries the language's `LangId`, the stream id of its first track in the
FULL item list, not the currently offered members. Initial focus
per page is an explicit id (root: active track, the Other languages row when the active track is
behind it, or Off; language page: active variant else first;
Other languages: the checked row else first; picker: the checked choice; Style: Size);
`opening_row` is only the fallback. LEFT/RIGHT stay on `EdgeRule::Screen` -> edge key; there is no
second ladder.

## Replay state

`PlayerOverlayArg` stays the opening address. The screen canonicalises tab, page path, selected
`RowKey` and each stacked page's saved return id; the SHAPE string changes and anchors are
re-recorded.

## Code structure

- **Table operations** (`ui/form.rs` `FormTable`, over `TableView`): `open` (snap, scroll 0,
  explicit initial id), `refresh` (same page, data changed: keep scroll and pill, restore by id, the
  pill slides), `refresh_with` (the same with a banked `prefer` id and a `fallback` id: prefer, then
  the id the table was on, then fallback, then the old-order neighbour) and `restore` (a pop: reinstate saved scroll and selection). `set` stays Settings'
  snap-and-reset. They differ because "the page changed", "its data changed" and "a page came
  back" want different scroll and pill behaviour.
- **Page stack** (`TrackMenuState::pages`, `TrackPage`): one `FormTable` serves the active page;
  a push saves the opener's `TrackRow` and the scroll, a pop restores both. To add a page: a
  `TrackPage` variant (with a `title` and a stable `code`), its form in `page_form`, its explicit
  `page_initial`, a Nav row whose `Dest` is the variant, and the `TrackRow`s it needs.
- **Form extensions**, reusable by Settings PRs 3-5 (`docs/settings-form.md`): a Nav row gets the
  chevron from its kind; Choice rows derive `checked` from a current-value predicate; an item can be
  disabled (dim, focusable, inert).
- **Title band** (`TableView::set_title`): "< TITLE" in the section-header caps caption, then a
  `DIV_H` divider gap, so it reads as the page's heading and not a caption glued to row one. The
  glyph is separated from the text by the check-column->label gap. It is part of `measured_height`,
  `measured_width` and `fit_report` (`FitRole::Title`). Its click target is the owner's, not the
  table's: a pointer-only key that pops, never in the D-pad column.
- **Wrapped notes**: `Row::note` wraps at the label column; the line count is resolved against the
  frame `Measure` (`TableView::fit_notes`), never read stale, and `fit_report` judges notes.
- **Composable clipping**: `ClipScope` (`DrawFrame::clip`, or `ClipScope::open_in` for a widget that
  holds only a `Painter`) is the ONE scissor stack: a nested scope INTERSECTS the enclosing one and
  restores it on drop. `TableView::draw` uses it, so two table draws can sit inside an outer panel
  clip. Painting and hit clipping share the mechanism; there is no second stack.
- **Pointer during transitions**: a surface-level pointer hold the dispatcher consults before hover
  and activation; while held, pointer input is swallowed (never a miss, so never dismiss).
- **Animation** (last PR): the page transition is internal to the page stack; the modal stack keeps
  appear/dismiss/input phases. Each page's layout is computed once when its inputs change; the spring
  animates only the panel rect and the two content layers' x offset and alpha under the clip stack.
  One panel background, no backdrop capture or transition textures. Key presses act on the logical
  state immediately (a push during a push retargets; push then pop mid-slide reverses).

## Rebuild signature

A live poll rebuilds the current page when any of these change: the subs fingerprint (count, stream
ids, offered sidecars), the active index, the renderer kind (text / image / ASS), transcoding, or the
own-burn / enhancement route and subtitle effect (what the Style rows' lock and Timing's omission read).
On the root the change refreshes in place. On a sub-page the page is refreshed in place too (focus kept by id) and pops to the root by id only when its availability no longer holds: the renderer kind changed, or Style's availability did (the own burn, or a server burn that omits it), or a page's own listing is gone (`TrackMenuState::pages_hold`: an Other languages page with no language left, a language page whose language, by stream id, is no longer in `other`).

## PR sequence

1. **Table groundwork:** wrapped notes with `Measure` at update, title band, composable clip,
   form.rs extensions and the three table operations; geometry and fit tests.
2. Keyed track forms (Audio + Subtitles), the page stack, Style pickers, persistence, locks, replay
   state, pointer and BACK behaviour.
3. Other languages, language pages, the badge change, the invalidation fingerprint.
4. Resize/slide animation for all table popovers (Tracks, More).
5. Later: More -> Quality drill-in.

## Decisions

- **Minimum width (owner):** the in-player popovers (Tracks, More) have a floor, `theme::layout::PLAYER_MENU_MIN_W` (440 px at 1080p), set on their `TableView` (`min_panel_w`), so a small page does not shrink to its labels. A floor only: wider content still grows the panel to `MENU_MAX_W`. At 440 the locked-renderer note takes two lines in English and Belarusian and three in Spanish (two needs ~520).
- **Geometry while the Position picker is open (owner): accept the overlap.** Panel bottom is fixed;
  at High the caption may pass behind the panel. No preview shift, no page-dependent HUD policy.
- **Evidence for the animation PR:** host tests for running vs resting (a transition reports
  animating and stops requesting frames at rest; push then pop reverses and settles); on device a
  frame-time capture during repeated push/pop with cues uploading, plus a motion capture the owner
  watches; an idle ceiling only on a plane-unbound fixture, because a bound video plane forces
  presents.
