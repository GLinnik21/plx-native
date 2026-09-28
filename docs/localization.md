# Help translate PlxNative

PlxNative's interface is available in English, Spanish and Belarusian (official Cyrillic
orthography). Help improve a translation or contribute another language through a pull request
at https://github.com/GLinnik21/plx-native.

## Translate a message

Catalogs live in `locales/en`, `locales/es` and `locales/be`, split by UI area. English is the
source: each scoped, stable key has a `value`, translator `description`, and optional typed
`args`. The other languages contain the same keys with translated string or plural values.
The compiler combines each language's files; file names organize ownership, not message identity.

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
may be omitted when the form spells it out. Belarusian needs `one`, `few`, `many`, and `other`;
Spanish needs `one`, `many`, and `other`; English needs `one` and `other`.

Use readable, natural wording. Belarusian switch labels use `Укл.` and `Выкл.`. Preserve the meaning of consent and privacy disclosures exactly.
Third-party license texts stay verbatim. Screenshots of the affected screen help reviewers assess
context and wrapping; do not include tokens, private server addresses or personal media data.

## Check and submit

Run `make check` to validate catalogs and run the host checks. Let it finish before starting
another build in the same checkout: its structure-gate tests temporarily modify source files. Every build rejects duplicate,
missing or unknown keys, invalid placeholders, and incomplete plural forms. The generated
accessors also make unknown message keys and wrong argument types compilation errors. Source
wording can change without renaming its key; change a key only when its meaning changes.

Some slots have a fixed width and line count, and a translation that overflows one ends in `…`.
Host tests measure each shipped language with the TV's own glyph widths and fail when such a line
would not fit. The covered slots are Settings row titles and subtitles, the sign-in failure
read-out, alert answer buttons, the player's More menu, the Library's TYPE menu, and the
collection page's meta line and season marks. When one of these tests fails, shorten
the wording; do not abbreviate it past readability.

Review the affected screens with the desktop simulator. `PLXNATIVE_LOCALE=es`, `be` or `en`
selects a language for a simulator launch; `PLXNATIVE_FORMAT_LOCALE=de-DE` exercises separate
regional formatting. `PLXNATIVE_LOCALE=qps-ploc` expands and accents text to reveal cramped
layouts. These environment overrides do not operate in the shipping TV build.

Open a pull request describing which language and screens changed, and how you checked the
wording and layout. Translations ship with app releases; the app does not download language packs.
New translations and changes remain subject to language review.

## Adding a language

Add a catalog directory using its BCP-47 language tag, translating all source keys. This currently
also requires a code change: extend the compiler's locale list and plural requirements, the
runtime language and preference enums, the Settings choices, and tests. Select CLDR plural rules
for that language, add launcher metadata, and verify every character in both bundled font faces.
The current renderer supports the shipped Latin and Cyrillic languages; Arabic/Hebrew and other
scripts requiring shaping or bidirectional layout need renderer work before being offered.

## Runtime behavior

Settings → Language defaults to the television's UI language. Explicit English, Spanish and
Belarusian choices apply the next time the app starts. The running UI and Plex request language
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
