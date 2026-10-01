# Drill-in sub-menus in the player track-menu popover

Design record for the multi-page Subtitles / Audio popover (Other languages, per-language pages,
Style pickers) and the table, form and clip primitives it stands on. Planned with an independent
model reviewer over several rounds; this file keeps the decisions and their reasons.

**Status:** PR 1 (table groundwork), PR 2a (the track menu on keyed forms) and PR 2b (the page stack in the Subtitles tab: Style, the Size / Position / Color pickers, nav keys, persistence, locks, the rebuild signature and replay state) are what this repository has; PRs 3-5 (languages and badges, animation, More -> Quality) are open. The replay anchors were re-recorded for 2b because the overlay's state shape changed on purpose.

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
- **Format badge:** only image subtitles (PGS/VobSub) show one; text subtitles show none.
- **Why no "active inside" readout on Other languages:** the active subtitle's language is in
  `yours` when the menu opens, so the active track does not live there. `yours` stays the opening
  snapshot so groups do not reshuffle under the viewer; if a live change puts the active track
  under Other languages, that row shows a leading check plus the active variant.

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

Each page declares rows with semantic ids (`Off`, `Track(sub_index)`, `OpenOther`, `OpenLang(group)`,
`Timing`, `Style`, `OpenField`, `Choice`, ...). Selection and saved openers are stored by id;
`RowKey` is a page base plus a STABLE ordinal (the track's index in the item's list, never a list
position). `OpenLang` carries `sub_layout`'s existing group identity and its first-track index comes
from the full item group, not the currently offered members. Initial focus per page is an explicit
id (root: active track or Off; language page: active variant else first; Other languages: the
checked row else first; picker: the checked choice; Style: Size); `opening_row` is only the
fallback. LEFT/RIGHT stay on `EdgeRule::Screen` -> edge key; there is no second ladder.

## Replay state

`PlayerOverlayArg` stays the opening address. The screen canonicalises tab, page path, selected
`RowKey` and each stacked page's saved return id; the SHAPE string changes and anchors are
re-recorded.

## Code structure

- **Three table operations** (`ui/form.rs` `FormTable`, over `TableView`): `open` (snap, scroll 0,
  explicit initial id), `refresh` (same page, data changed: keep scroll and pill, restore by id, the
  pill slides) and `restore` (a pop: reinstate saved scroll and selection). `set` stays Settings'
  snap-and-reset. They differ because "the page changed", "its data changed" and "a page came
  back" want different scroll and pill behaviour.
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
A page whose availability or `OpenLang` target no longer holds pops to the root by id.

## PR sequence

1. **Table groundwork:** wrapped notes with `Measure` at update, title band, composable clip,
   form.rs extensions and the three table operations; geometry and fit tests.
2. Keyed track forms (Audio + Subtitles), the page stack, Style pickers, persistence, locks, replay
   state, pointer and BACK behaviour.
3. Other languages, language pages, the badge change, the invalidation fingerprint.
4. Resize/slide animation for all table popovers (Tracks, More).
5. Later: More -> Quality drill-in.

## Decisions

- **Geometry while the Position picker is open (owner): accept the overlap.** Panel bottom is fixed;
  at High the caption may pass behind the panel. No preview shift, no page-dependent HUD policy.
- **Evidence for the animation PR:** host tests for running vs resting (a transition reports
  animating and stops requesting frames at rest; push then pop reverses and settles); on device a
  frame-time capture during repeated push/pop with cues uploading, plus a motion capture the owner
  watches; an idle ceiling only on a plane-unbound fixture, because a bound video plane forces
  presents.
