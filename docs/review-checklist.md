# Review checklist: checks a diff triggers

Each item is a bug that reached `main` once and that the diff alone could have shown. An item has a
TRIGGER (something readable from the diff), a CHECK (the question to answer by reading code, not by
guessing) and the pull request it came from with the commit that fixed it. `pr-reviewer` reads this
file from the base, matches the triggers against the diff, and runs only the items that match; most
diffs match none or one. An item that matches and passes is not reported.

Add an item only when a fix names the pull request that caused the bug, the cause is readable in
that pull request's diff, and `tools/review-eval.py` holds the case; say so in the same pull request.
Delete an item once a gate in `make check` enforces it, or when the eval shows it flags controls and
catches nothing. The list is meant to stay short: a long one is skimmed.

| # | Trigger in the diff | Check | From |
|---|---|---|---|
| 1 | A function or symbol that was linked directly becomes a runtime lookup (`dlsym`, `dynlib!`, a `Handle` lookup, an `Option` of a function pointer). | For every caller: what does it do on a firmware where the lookup finds nothing? A call that turns into a silent no-op is a finding unless the diff gives that firmware another way to get the behaviour. Name the firmware and the lost behaviour. | #578, fixed by #655 |
| 2 | A language is added or removed (`locales/languages.json`, a `locales/<code>/` directory, `i18n::SHIPPED`). | Search outside `locales/` for every table keyed by language code (`tools/`, `tests/`, `site/`, test corpora such as `LONG_TITLES` in `tools/hud_preview.py`). Each one needs the new language, or the tool that reads it refuses to run. | #1230, fixed by #1253 |
| 3 | Boot or startup code creates, opens or initialises something (a native handle, a connection, a worker) that only an optional feature or a non-default engine uses. | Is it created for a viewer who never uses that feature? It belongs at first use. On the television a second native handle or client can change how the firmware treats the first, which no host test shows. | #1281, fixed by #1396 |
| 4 | A menu or list row is hidden, removed or reordered under some condition. | In that condition, which row is now first, and where does focus sit when the menu opens or when the focused row disappears? A destructive row (Sign out, Remove, Mark as Unwatched, Delete) there is a finding. | #237, fixed by #242 |
| 5 | Text is added to a file that is compiled into the binary whole (a shader behind `glsl!`, anything behind `include_str!` or `include_bytes!`), comments included. | Does it spell a dev-trigger name (`plxnative-…`) or another string the packaged-binary scan (`ci/check-package.py`) refuses in a release build? | #981, fixed by #994 |
| 6 | A landing, settle or timeout rule starts waiting for a callback, counter or event that the firmware sends. | Does every supported set send it? Say what the rule does on a set that never does: if it runs out a timer and then reloads, retries or fails something that was working, that is a finding. | #1313, fixed by #1437 |
| 7 | Reader-visible text is added under `site/`. | Is every character inside the site font subset (`BASE_RANGES` in `tools/subset-site-fonts.py`)? A character outside it fails the subset check and stops the Pages build. | #1252, fixed by #1277 |
