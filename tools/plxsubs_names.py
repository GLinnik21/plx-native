"""The name-record aliases that make libass find the subtitle fonts (plan D6).

Shared by `tools/cut-inter.py` (which writes `pkg/appfont-bold.ttf`) and `tools/cut-noto-subs.py`
(which writes `pkg/fonts/subs/*.ttf` and re-applies the Inter alias idempotently), so a re-cut of
either cannot silently drop it. `rust-modules/media/src/player/ass/fonts.rs` asserts the result on
the shipped files.

libass reads every Microsoft-platform name record with ID 1 (family) and ID 4 (full name) of a font,
whatever its language (`get_font_info`, ass_fontselect.c): an extra record makes the face answer to
a second family name. The records need three distinct languages, because fontTools refuses two
records with the same (platform, encoding, language, nameID):

  * English 0x0409 keeps the real name ("Inter", "Noto Sans Thai"), which UI tools and FreeType see.
  * German 0x0407 carries "PlxSubs" - the family the synthesized subtitle style asks for.
  * French 0x040C carries "Inter" on the Noto faces - the fallback family authored ASS names.

The languages are deliberately NOT English variants. FreeType's `tt_face_get_name` (sfobjs.c) takes
the first Windows record it finds and then replaces it with any later one whose
`languageID & 0x3FF == 0x009`; an alias under 0x0809 or 0x0C09 could therefore become the
`family_name` FreeType (and `gfx/text.rs`) reports for the file.
"""

ALIAS_FAMILY = "PlxSubs"
LANG_PLXSUBS = 0x0407  # de-DE
LANG_INTER = 0x040C    # fr-FR
PLATFORM_WINDOWS, ENCODING_UNICODE_BMP = 3, 1
NAME_FAMILY, NAME_FULL = 1, 4


def english_variant(lang_id: int) -> bool:
    """What FreeType's `tt_face_get_name` treats as an English record."""
    return (lang_id & 0x3FF) == 0x009


def _set(font, name: str, name_id: int, lang_id: int) -> None:
    font["name"].setName(name, name_id, PLATFORM_WINDOWS, ENCODING_UNICODE_BMP, lang_id)


def add_plxsubs_alias(font) -> None:
    """Inter Bold answers to "PlxSubs" (family and full name) under langID 0x0407."""
    for name_id in (NAME_FAMILY, NAME_FULL):
        _set(font, ALIAS_FAMILY, name_id, LANG_PLXSUBS)


def add_noto_aliases(font) -> None:
    """A Noto subtitle face answers to "PlxSubs" (0x0407) and "Inter" (0x040C) as well."""
    add_plxsubs_alias(font)
    for name_id in (NAME_FAMILY, NAME_FULL):
        _set(font, "Inter", name_id, LANG_INTER)
