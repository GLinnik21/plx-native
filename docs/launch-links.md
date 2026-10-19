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
- Sign-in, the profile picker and onboarding are never interrupted: the link waits behind the
  picker and onboarding, and is dropped when no account is signed in.
- The debug flavour's id is `com.beb.plxnative.debug`; the contract is the same.

## Close the app first

Only a cold launch carries the link. SAM hands a cold start's parameters to the process as
`argv[1]`, but launched again while it runs, SAM sends them as a `relaunch` event to the
`registerApp` subscription, which LG's SDL holds and which raises the window and drops them. The
app cannot hold that subscription itself (measured on webOS 9.2.2, LG C2: SAM answers an anonymous
client `cannot find caller id`, the app-id service name is already taken in the process, and
`LSCallFromApplication` is refused as unprivileged). So a caller that may find PlxNative running
closes it first, then launches:

```text
luna://com.webos.applicationManager/closeByAppId {"id":"com.beb.plxnative"}
```

Leave a short pause between the two so the close finishes first; 600 ms worked on the LG C2.
