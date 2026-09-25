# screens/ — the application's owned screens (read before adding or changing one)

Every `Screen` impl the dispatcher mounts, steps, focuses and draws lives here: 82 files across
this directory and the `detail/`, `home/`, `library/`, `player/` and `search/` families. This is
the APPLICATION half of the restructure; the library half is `../ui/`, whose
[`CLAUDE.md`](../ui/CLAUDE.md) carries the token rules, the architecture and the rendering
policy. Read that one first — its four rules bind here too.

**`mod.rs` and `registry.rs` already document themselves.** Both carry substantial `//!` docs and
they are the authority on the module list and the application bundle. This guide holds only what
they do not: the rules that span screens.

## The layer rule is the thing that will bite you

A screen may name `ui/`, `stores/`, the data crates and this directory's `registry` — **never
`app/`, and never a sibling screen module**. `ci/check-deps.sh`'s `layer` and `sibling` gates have
held this since restructure phase 10, so a violation fails CI rather than review.

Two consequences worth stating, because both look like the obvious thing to do:

- **Reaching another screen is a message, not a call.** *Also available* reports its destination
  rather than navigating: it delivers `AppMsg::AltSourceOpen` to the Detail instance named on its
  argument, and the mounted `DetailScreen` turns that into a `ContentReq::Present`.
- **A screen keeps no focus of its own.** The `FocusEngine` in `Input` holds THE current focus and
  every group's remembered cursor (spec §7.3). There is no per-screen `move_focus`, `top_focus` or
  `key(sym)` ladder any more, and adding one back is how the old twelve-ladder tangle started.

No `static mut` here either: a screen's state is a field of the instance the container owns (§6.1).

## Adding a screen

The spec's done-criterion 5 is a measured claim, not an aspiration: *a new screen touches its own
`screens/<name>.rs`, `screens/registry.rs`, `dev/scenarios.rs` and `tests/manifest.json` and
nothing else.* Two conversions were run to prove it, and
[`docs/ui-system-migration.md` §(E)](../../../docs/ui-system-migration.md) records what
`git diff --name-only` actually said for each — the real answer is those four plus `screens/mod.rs`,
`ui/mod.rs` and `ci/allow/statics-migration.txt`.

If your change is reaching further than that list, the design says stop and ask why: the variant,
the screen id, the `mount` arm and the recorded shape (`SCREEN_SHAPES`) all live in `registry.rs`
precisely so they do not spread.

A screen mounted as a modal surface is the same trait with a different `Style`; the Settings family
instantiates the same screens a second time for its own inner stack, which is how one
`OnboardScreen` mounts twice (§6.2).

## A landing must not move ground somebody is looking at

The Library's seven reported bugs in 2026-09 had one cause, and the shape recurs on every screen
that mixes async content with a live cursor:

- **Stage, never self-commit.** `browse::section_hubs`' `land_ok` ALWAYS stages. The first paint
  used to self-commit "because there is no layout yet to protect" — but the window is four
  SECONDS, and two presses reach the grid inside it.
- **A reloading control is not a missing control.** `browse::requery` empties the store
  synchronously on the press, so for the length of the fade the heading, the Sort/Filter row and
  the rail are all controls acting on nothing, and the async focus clamp *correctly* moved focus
  off an undrawn zone. The clamp cannot tell "this control is gone" from "the thing it acts on is
  reloading" — so `Layout::grid_head` stays true while a grid-scoped transition is in flight.
- **A seat nobody chose follows the head.** A page that seats its own focus on its document's
  first block must re-seat when a landing changes what that block is, until the user moves. The
  Library's grid is usually prepared before its shelves land, so its first seat was the grid's
  heading; the shelves then committed above it and the first DOWN went on into the grid
  (`LibraryScreen::provisional`). Home already has this shape (the hero seats when its action row
  arrives, while focus is still on the strip); Search seats its field, which never arrives async,
  and Person and Detail keep no self-seat for a landing to strand.
- **Derive re-entry position, don't store it.** `Layout::seat_for_scroll` derives the focus seat
  from the restored SCROLL rather than a saved grid index: the server's hubs change subject
  between requests, so a stored shelf index can name a different shelf on return.

`library/` reports whether the focus STOP moved so a press can be cancelled on hover;
`home/`, `person` and `search` still carry that gap. It is a known hole, not a pattern to copy.

## Verifying a screen change

Captures are the check — see the `ui-sim` and `which-tier` skills, and `../ui/CLAUDE.md`'s
"When you're done". Two traps specific to this directory:

- A screen's `name()` is the heartbeat word and must stay byte-identical to the route word the
  test manifest selects on (spec §15.3). Renaming it silently unselects every fps scene for that
  screen.
- The person page's bio provider 401s on an injected token, so a headless boot cannot reach it —
  sign in, or drive that leg a different way.
