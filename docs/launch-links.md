# Launch links

Another app on the television (a launcher's Continue Watching row, say) can open a Plex title in
PlxNative by passing it in the launch parameters. The code is `rust-modules/src/app/deeplink.rs`;
the argument plumbing is `rust-modules/platform/src/tv/launch.rs`.

## The contract

```text
luna://com.webos.applicationManager/launch
{"id":"com.beb.plxnative","params":{"plex":{"server":"<machineIdentifier>","ratingKey":"12345","action":"open"}}}
```

| Key | | Meaning |
| --- | --- | --- |
| `ratingKey` | required | The item, as a string or a number; digits only. |
| `server` | optional | The server's `machineIdentifier`. Without it, the signed-in current server. |
| `action` | optional | `open` (the default) lands on the item's detail page; `play` starts it where Resume would. Anything else is `open`. |

- Unknown keys are ignored. A launch with no `plex` object is an ordinary launch, unchanged.
- The page opens with Home alone underneath it, so BACK returns to Home.
- A key the server does not know, a server that never appears, or a malformed `plex` object
  leaves Home as it is and writes a `deeplink:` line to the event log; nothing is shown on screen.
- Sign-in, the profile picker and onboarding are never interrupted. A TV that is signed out keeps
  the link through sign-in and acts on it once Home is the root. A link that names a server
  resolves against the account that signed in (it waits up to 15 s for that server to appear in
  the roster, then writes `deeplink: not opened (server not available)`). A link that names no
  server cannot know which account it was meant for after a sign-in, so it is dropped, with a
  `deeplink: dropped` line.
- The link also lands only when no surface is up (the first-run consent question, a menu) and the
  app is in the foreground; it waits for the surface to be dismissed and the window to be
  foregrounded again. Ending playback (next bullet) waits for the window but not for a surface.
- A link over playback ends it the way the STOP key does (the player's exit ritual: the engine is
  released, the server is told, a parked restore is retired) and then lands. It does this as soon
  as the link is due and the window is up. A link that arrives while the app is in the background
  waits for the foreground and then does both at once, so a film that was parked is not brought
  back first and nothing is torn down while the window is hidden. If the engine does not end within 5 s the link is dropped with
  `deeplink: not opened (playback did not end)`. A link whose server never resolves never stops
  the film.
- `action: play` starts through the same door as a Play from a page. While a trailer preview is
  still letting go of the engine the play waits; if the engine is still busy when the wait's 12 s
  ceiling passes, the link is dropped with `deeplink: not opened, rk=… (engine busy)`.
- The debug flavour's id is `com.beb.plxnative.debug`; the contract is the same.

## A launch while the app runs

SAM hands a cold start's parameters to the process as `argv[1]`. Launched again while it runs, SAM
sends them as a `relaunch` event to the `registerApp` subscription, which LG's SDL holds; SDL raises
the window and never reads the parameters. The app cannot hold that subscription itself (measured
on webOS 9.2.2, LG C2: SAM answers an anonymous client `cannot find caller id`, the app-id service
name is already taken in the process, and `LSCallFromApplication` is refused as unprivileged).

Instead, `app::boot::tap_relaunch_payloads` wraps the pointer slot LG's SDL reads each reply's
payload through (`HELPER_HLunaServiceMessage`, exported by its `libSDL2` and filled in `SDL_Init`).
A relaunch payload is passed to the link code and returned unchanged, so SDL still raises the
window. The link then replaces wherever the app was, with Home underneath. A relaunch without a
`plex` object leaves the app where it is, and does not displace a link that no frame has taken yet. The tap is installed only where the slot holds the
address of libhelpers' `HLunaServiceMessage`; anywhere else it is not installed and the event log
says so, so a warm link works only where the guarded tap installs. Measured on webOS 9.2.2 (LG C2): links opened from Home
and from another detail page, on the same process.

## Where the slot is missing

The firmware inventories (`tools/fwcompat.py`) have the slot in LG's SDL from webOS 7.4 on, and not
on webOS 4.x to 6.x. Where it is missing, the event log says `relaunch tap: no
HELPER_HLunaServiceMessage in this SDL` at boot and only a cold launch carries the link. A caller
that must work there closes the app first, then launches:

```text
luna://com.webos.applicationManager/closeByAppId {"id":"com.beb.plxnative"}
```

Leave a short pause between the two so the close finishes first; 600 ms worked on the LG C2.
