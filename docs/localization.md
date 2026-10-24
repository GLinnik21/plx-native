# Help translate PlxNative

PlxNative's interface is available in English, Spanish, French, Italian, European Portuguese, German, Hungarian, Russian and Belarusian
(official Cyrillic orthography). Help improve a translation or contribute another language through a pull request
at https://github.com/GLinnik21/plx-native.

## Translate a message

Catalogs live in `locales/en`, `locales/es`, `locales/fr`, `locales/it`, `locales/pt`, `locales/de`, `locales/hu`, `locales/ru` and `locales/be`, split by UI area. English is the
source: each scoped, stable key has a `value`, translator `description`, and optional typed
`args`. The other languages contain the same keys, except the English-only ones (described
below), with translated string or plural values.
The compiler combines each language's files; file names organize ownership, not message identity.

Some English entries carry `"translate": false`. They are technical read-outs meant for the
people who debug the app — the Stats-for-nerds diagnostics panel, its playback verdict line and
the support codes on failure screens — and they show in English in every language. Do not add
them to your catalog; the build rejects a catalog that contains one. Everything else, including
consent and privacy text that looks technical, is translated.

```json
{
  "library.results": {
    "value": {"one": "{count} result", "other": "{count} results"},
    "args": {"count": "i64"},
    "description": "Number of results above the library grid"
  }
}
```

Spanish would provide:

```json
{
  "library.results": {
    "one": "{count} resultado",
    "many": "{count} resultados",
    "other": "{count} resultados"
  }
}
```

Keep placeholder names unchanged, but reorder them to suit the language. Translate the complete
phrase. Never translate protocol identifiers, URLs, brand names or server-returned media titles.
Double braces (`{{` and `}}`) render literal braces. A placeholder accepts `str` or `i64`;
integers are formatted with the TV's regional settings. A plural message's `count` selects a
CLDR cardinal category for the UI language. Other arguments must appear in every form; count
may be omitted when the form spells it out. Each language needs exactly the categories its
`plurals` entry in `locales/languages.json` declares, no more: Belarusian and Russian `one`, `few`, `many`, and
`other`; Spanish, French, Italian and Portuguese `one`, `many`, and `other`; English, German and Hungarian `one` and `other`.

Use readable, natural wording. Belarusian switch labels use `Укл.` and `Выкл.`, Russian `Вкл.` and `Выкл.`. Preserve the meaning of consent and privacy disclosures exactly.
Third-party license texts stay verbatim. Screenshots of the affected screen help reviewers assess
context and wrapping; do not include tokens, private server addresses or personal media data.

## Check and submit

Run `make check` (or, while iterating, `make test-crate C=plx_platform T=catalog`) to validate catalogs and run the host checks. Let it finish before starting
another build in the same checkout: its structure-gate tests temporarily modify source files. Every build rejects duplicate,
missing or unknown keys, invalid placeholders, incomplete plural forms, or a translation of an English-only message. The generated
accessors also make unknown message keys and wrong argument types compilation errors. Source
wording can change without renaming its key; change a key only when its meaning changes.

Some slots have a fixed width and line count, and a translation that overflows one ends in `…`.
Host tests measure each shipped language with the TV's own glyph widths and fail when such a line
would not fit. The covered slots are Settings row titles and subtitles, the sign-in failure
read-out, alert answer buttons, the player's More menu, Subtitles panel and subtitle-timing control,
the Library's Sort & Filter menu, and the
collection page's meta line and season marks. When one of these tests fails, shorten
the wording; do not abbreviate it past readability.

Review the affected screens with the desktop simulator. `PLXNATIVE_LOCALE=es`, `fr`, `it`, `pt`, `de`, `hu`, `ru`, `be` or `en`
selects a language for a simulator launch; `PLXNATIVE_FORMAT_LOCALE=de-DE` exercises separate
regional formatting. `PLXNATIVE_LOCALE=qps-ploc` expands and accents text to reveal cramped
layouts; English-only diagnostics text is not expanded. These environment overrides do not operate in the shipping TV build.

Open a pull request describing which language and screens changed, and how you checked the
wording and layout. Translations ship with app releases; the app does not download language packs.
New translations and changes remain subject to language review.

### Who reviews a translation

A translation is accepted when a native speaker of that language has read it. Machine or AI-drafted
translations are welcome as a starting point, but say so in the pull request and name who reviewed
the wording (the author, or someone they asked). A pull request from a non-native speaker waits
for a native reviewer before it is merged.

The reviewer does not need to build anything. Send them the catalog files in `locales/<tag>/` and
screenshots of the affected screens (desktop simulator with `PLXNATIVE_LOCALE=<tag>`, or the TV),
so they can see each string in its place: the same words read differently in a button, a menu and
a sentence. Note any wording they changed and why.

## Adding a language

A UI language is `locales/<tag>/` plus its entry in `locales/languages.json`.
Month-and-day captions also need the language's abbreviated and full format-month names in
`LocaleContext::month_day` (`rust-modules/platform/src/i18n/mod.rs`), with tests for both widths.
Use the grammatical form that follows a day number (for Russian, `12 марта`, not `12 март`).

1. Add the catalog directory, named by the bare language tag (`it`, not `it-IT`), translating all
   source keys. A translation is a bare string or a plural map; `value`, `description` and `args`
   belong only in `locales/en`.
2. Add one line to `locales/languages.json`: `tag`, `native_name` (the name in that language, shown
   in Settings), `region` (`<tag>-XX`, the formatting locale), `script` (`Latn` or `Cyrl`),
   `plurals` (the language's CLDR cardinal categories), optionally `excluded_regions` (region codes
   of this language that keep the English fallback because no catalog serves them: Portuguese
   excludes `BR`, so a pt-BR television shows English while pt, pt-PT and pt-AO show Portuguese;
   the build rejects anything but two upper-case letters) and `row` (an unused number from 1 to 999,
   in list order; a shipped language never changes its row or tag). A test compares `plurals` with
   ICU's own data, and another checks that ICU has the language's number and plural data.
3. Add launcher metadata, `pkg/resources/<tag>/appinfo.json`, and verify every character in both
   bundled font faces (the build tests do).
4. Run `make test-crate C=plx_platform T=catalog`. A directory with no registry entry, an entry with
   no directory, a missing or extra key, a missing or never-selected plural form and a wrapped
   translation are build errors that name the file and key, all listed at once.

The rule for code: a UI language is its directory and registry entry. Code never lists languages:
every "each language" loop uses `i18n::SHIPPED` (`ci/check-language-lists.py` fails a hand-written
list), and production code never `match`es on `Language` or `Preference` outside `i18n`. A test that
pins a reviewed translation iterates its own table and never falls back with `_ =>`. The current
renderer supports the shipped Latin and Cyrillic languages; Arabic/Hebrew and other scripts
requiring shaping or bidirectional layout need renderer work before being offered.

## Runtime behavior

Settings → Language defaults to the television's UI language. Explicit English, Spanish, French, Italian,
Portuguese, German, Hungarian, Russian and Belarusian choices apply the next time the app starts. The running UI and Plex request language
remain unchanged until then; playback is not interrupted. The contribution action displays this
guide's URL and QR code for opening on another device.

At boot, the native Settings Service supplies `localeInfo.locales.UI`, `locales.FMT` and `clock`.
UI resolution is saved override, TV UI language, process locale, then English. Unsupported UI
languages use English. Missing formatting settings use the resolved UI language's regional
default. Numeric Gregorian dates and numbers use the formatting locale; playback clocks remain
durations. Locale data and catalogs are bundled, so translating the UI requires no network.

Plex requests carry the resolved UI language in `X-Plex-Language`. Servers may still return their
own stored language. This preference does not change media audio/subtitle selection or library
sorting. Launcher metadata follows webOS's system language independently of the app override.

Controlled record/replay runs use the captured session's language preference; System resolves to
English and regional formatting is pinned to en-US/24-hour in both modes. They do not read live
TV settings or simulator language environment variables. Use ordinary simulator launches for
language screenshots.

## Verification of the initial implementation

On 2026-09-27, the host suite, shipping-feature type check, ARM build, ELF audit and supported
firmware loader matrix passed. Simulator captures covered English, Spanish, Belarusian and the
expanded pseudo-locale across browsing, detail, search, sign-in, language settings, privacy,
contribution and consent screens. In that initial version, long consent actions reflowed and
disclosures remained scrollable at the standard body size. The later design correction uses
concise answer verbs in one measured row and allocates more space to the disclosure.

The installed native debug app on the webOS 4.5 television successfully queried the Settings
Service. Remote-driven language selection remained unchanged during the running session and
persisted across relaunches into Belarusian and Spanish, independently of the TV formatting
locale. Native captures verified Cyrillic text, abbreviated switch values, complete consent
questions, disclosure scrolling and the contribution page. The QR code decoded to the guide URL.
The device preference was restored to System default afterward.

These checks cover localization and UI behavior. They do not replace decoder/playback tests,
and a Plex server may still return metadata in its own stored language.
