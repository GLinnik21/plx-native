# Nightly builds

A nightly is the newest PlxNative build from `main`, published on most days the code changes. It
installs **beside** the regular app as **PlxNative Nightly** - its own launcher tile and its own
sign-in - so trying it never touches the release you already trust.

**A nightly is not tested on a television.** It passes the same automated build and packaging
checks a release does, but nobody has watched it play. Keep the regular PlxNative installed, and
expect rough edges. New work reaches a nightly before it reaches a release, which is the point.

The simplest way to follow nightlies is the project's own Homebrew Channel repository: add it once,
install PlxNative Nightly, and every new build appears as an update on the TV.

## What you need

- A TV that already runs the regular PlxNative. The [installation guide](install-and-verify.md)
  covers LG Developer Mode and Homebrew Channel; nothing extra is needed for nightlies.
- **Homebrew Channel** on the TV. Without it, [install by hand](#install-by-hand-without-homebrew-channel).
- The TV's Developer Mode session kept alive, as for any app installed through it. It
  [expires and removes the apps](install-and-verify.md#important-developer-mode-expires) unless you
  renew it - the nightly is no exception.

## Install with Homebrew Channel

Do this once, on the TV:

1. Open **Homebrew Channel** and go to **Settings**.
2. Choose **Add repository** and enter:

   ```
   https://plxnative.com/nightly/repo.json
   ```

3. Go back to the app list. **PlxNative Nightly** now appears beside the other entries.
4. Open it and select **Install**. Homebrew Channel checks the download's checksum before it
   installs anything.
5. Open **PlxNative Nightly** from the launcher and sign in. It does not share your sign-in or
   settings with the regular app.

## Updating

Each time a new nightly is published, its entry in Homebrew Channel offers **Update**. Select it to
install the new build over the old one. Updates happen when you select them - nothing installs by
itself.

Homebrew Channel offers an update whenever the version it lists differs from the one installed, so
the version shown in its list is the nightly's package version (for example `0.8.20261006`), not
the longer version the app shows about itself. [The versions section](#versions) explains both.

New builds appear once the day's build has finished publishing; the site's cache can add up to
ten more minutes. There is at most one nightly a day, and none on days when nothing
outside the website and docs changed.

## Install by hand (without Homebrew Channel)

If you do not use Homebrew Channel, or want a specific build:

1. Open [plxnative.com/nightly/latest.json](https://plxnative.com/nightly/latest.json). It names
   the newest nightly, its `.ipk` download and its SHA-256 checksum.
2. Download the `.ipk` and [verify the checksum](install-and-verify.md#verifying-the-package).
3. Install it with webOS Dev Manager, as in the [installation guide](install-and-verify.md).

A package installed this way does not update itself. Install the next one the same way.

## Going back

- To stop following nightlies, uninstall **PlxNative Nightly** from Homebrew Channel, then remove
  the repository under **Settings** if you no longer want it listed.
- The regular PlxNative is separate and is not affected either way.
- Nightly builds are deleted from the download page 30 days after they are published. The newest
  one is always kept. A copy already installed on your TV keeps working.

## What is different about a nightly

- **Its own app.** Its id is `com.beb.plxnative.nightly`: separate tile, separate sign-in,
  separate saved state. Nothing is shared with the regular app.
- **Untested on a TV**, as above.
- **Error reports go elsewhere.** If you have turned on crash reports or usage analytics, a nightly
  sends them to the project's development tracker, kept apart from what the regular app sends.
  They are off until you turn them on, exactly as in the regular app.
- **It reports a dated version**, which is what to quote when you report a problem.

## Versions

The regular app has one version, `X.Y.Z`. A nightly has two, because the TV's installer and the
app itself need different things from a version.

| What | Looks like | Where you see it | What it is for |
| --- | --- | --- | --- |
| Reported version | `0.8.0-nightly-20261006` | About screen, diagnostics, bug reports | Telling people which build you ran. **Quote this.** |
| Package version | `0.8.20261006` | Homebrew Channel, the TV's installer | Telling the installer a build is different and newer. |

The reported version reads **`<release it is heading to>-nightly-<date it was cut>`**. The package
version keeps the same `major.minor` and uses the **date as its patch number**. A real patch
number is small, so a long one always means a nightly. Because the date only goes up, each nightly
is a newer package than the one before.

How releases are told apart:

| Release | Cut from | Version moves | The nightlies after it |
| --- | --- | --- | --- |
| Minor (the normal case) | `main` | `0.7.0` to `0.8.0` | head for the next minor: `0.9.<date>` |
| Patch | a maintenance branch (`release/vX.Y`) | `0.7.0` to `0.7.1` | `main` is unaffected; nightlies still come only from `main` |
| Major | `main`, bumped by hand | `0.x.y` to `1.0.0` | head for `1.1.<date>` |

Some consequences:

- **No downgrade across a release.** A release raises the version the next nightly heads for, so
  the package version keeps rising straight through it.
- **Nightlies come only from `main`.** A build from a maintenance branch would carry a lower
  version than one you already installed, so the pipeline refuses to publish one.
- **At most one nightly a day**, because the date is the whole build number.
- **The package version is not the commit.** The commit a build came from is in its release notes
  and on its description page in Homebrew Channel.

## If something goes wrong

- **The repository shows no apps.** No nightly with a manifest has been published yet, or the site
  is mid-update. Try again in a few minutes.
- **Homebrew Channel will not take the URL.** Check it character by character, including `https://`
  and the `.json` ending. Typing with the TV remote makes this the most common slip.
- **Install or update fails.** Note the exact message, the version Homebrew Channel lists and your
  TV model and webOS version, and [open an issue](https://github.com/GLinnik21/plx-native/issues).
  Installing nightlies through Homebrew Channel is new, so a failure here is worth reporting.
- **The app is gone after a while.** That is the Developer Mode session expiring, not the nightly.
  See the [installation guide](install-and-verify.md#important-developer-mode-expires).

## Reporting problems

[Open an issue](https://github.com/GLinnik21/plx-native/issues/new) and include the **reported
version** from the About screen, for example `0.8.0-nightly-20261006`, with your TV model and webOS
version. Do not post passwords, Plex tokens or screenshots that show them.
