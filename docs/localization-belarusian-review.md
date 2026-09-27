# Belarusian language review

Reviewed on 2026-09-27 against [Skarnik](https://www.skarnik.by/), as requested.
The review covered all 556 entries present at its start: `browse.json` (286),
`core.json` (16), `settings.json` (140), and `widgets.json` (114), including every
plural branch and the long consent, privacy, and legal passages. The review
considered grammar, repeated terminology, and the context of navigation, people
and filmography, playback controls, consent, and deletion actions.

Dictionary checks below establish particular word forms or meanings. Sentence edits
are editorial grammar judgments, identified separately from dictionary evidence.
The source limitations for specialist terms are recorded below.

## Changes made

| Catalog key | Change and reason | Evidence |
| --- | --- | --- |
| `settings.consent.share_reports`, `settings.consent.share_analytics` | Use **Дасылаць** for both affirmative actions and **Не дасылаць** for the negative action, following the language review supplied on September 28. Each visible question already names the reports or analytics, so the actions need not repeat that object. | Skarnik confirms [дасылаць](https://www.skarnik.by/belrus/26647). The verb preference and contextual shortening are editorial choices; the former **адпраўляць** was not a dictionary error. |
| `browse.detail.direct_stream` | Use **Прамы паток**, matching `widgets.playback.direct_stream` for the same Plex playback mode. The former **Прамая трансляцыя** could suggest a live broadcast. | [поток → паток](https://www.skarnik.by/rusbel/69587); distinguishing this Plex mode from broadcasting is an application-context judgment. |
| `settings.legal.privacy.local_body` | Remove the comma before the final **і** in the list of locally stored settings. | Sentence-level punctuation correction; not a dictionary claim. |
| `settings.legal.trademarks.body`, `settings.about.body` | Give the creation/endorsement verbs their own explicit subject, the named companies, and put non-affiliation in a separate sentence. The former coordination attached **з** to predicates that need different complements. The same correction is applied in both repeated passages. | [создать → стварыць](https://www.skarnik.by/rusbel/88806), [одобрить → ухваліць](https://www.skarnik.by/rusbel/52410); the sentence construction is an editorial grammar correction. |

Separately, `settings.read_details` and `settings.scroll_details` were removed
alongside the explicitly requested removal of their UI hints. This leaves 554
Belarusian entries. Their removal is a feature change, not a language finding.

## Checked wording retained

The initial dictionary pass did not substitute alternatives merely because another synonym was possible.
These existing terms have direct support in Skarnik:

| Existing wording | Skarnik evidence |
| --- | --- |
| **даныя**, including **даных** in privacy/deletion copy | [данные](https://www.skarnik.by/rusbel/17095) |
| **Выдаліць** | [удалить](https://www.skarnik.by/rusbel/97033), removal sense |
| **Скасаваць** | [отменить](https://www.skarnik.by/rusbel/55860), decision sense; the entry also gives **адмяніць** |
| **справаздача** | [отчёт](https://www.skarnik.by/rusbel/57013) |
| **згода** | [согласие](https://www.skarnik.by/rusbel/88682) |
| **фільм** | [фильм](https://www.skarnik.by/rusbel/100029) |
| **акцёр** | [актёр](https://www.skarnik.by/rusbel/1098) |
| **рэжысёр** | [режиссёр](https://www.skarnik.by/rusbel/81368) |
| **сцэнарыст** | [сценарист](https://www.skarnik.by/rusbel/92632) |
| **праца** | [работа](https://www.skarnik.by/rusbel/77257); its use for a filmography credit is contextual, not a dictionary-provided UI translation |
| **прайграваць** | [проигрывать](https://www.skarnik.by/rusbel/74334), performing/playing sense |
| **раздзел** | [глава](https://www.skarnik.by/rusbel/15328), division of a work |
| **пазначыць** | [пазначыць](https://www.skarnik.by/belrus/62798), marking sense |
| **разрозненне** | [разрешение](https://www.skarnik.by/rusbel/79044), resolving-capacity sense |
| **слой** | [слой](https://www.skarnik.by/rusbel/87759) |
| **сканіраваць** | [сканировать](https://www.skarnik.by/rusbel/86644) |

The user-approved **Укл.** and **Выкл.**, including their periods, are unchanged.
Skarnik supports their underlying verbs [уключыць](https://www.skarnik.by/rusbel/9175)
and [выключыць](https://www.skarnik.by/rusbel/12462); the abbreviated UI forms and
punctuation are an explicit product decision, not something inferred from those
dictionary entries.

The actual library screenshot reads **21 фільм**, which is correct. The catalog's
film-count branches and ICU plural selection were left unchanged. Dictionary
lookup of the noun does not replace checking the plural rule or the rendered
count.

## Dictionary limits and unresolved terminology

- Skarnik's bilingual dictionaries give **субтытр**
  ([Russian–Belarusian](https://www.skarnik.by/rusbel/91998),
  [Belarusian–Russian](https://www.skarnik.by/belrus/92823)), while its explanatory
  dictionary explicitly contains **субцітр** with the relevant cinema meaning
  ([entry](https://www.skarnik.by/tsbm/80798)). The existing **Субцітры** is retained
  throughout. This disagreement within the requested source is documented rather
  than presented as proof of a spelling error.
- An exact **фільмаграфія / фильмография** entry was not found in the bilingual
  lookups. The existing heading is retained; this review does not claim Skarnik
  directly verified it.
- The dictionary supports [прыватны](https://www.skarnik.by/belrus/76860) in the
  personal/private sense, but that does not by itself certify the complete modern
  UI phrase **Палітыка прыватнасці**. The existing consistent terminology remains.
- Specialist forms such as remuxing, native crash capture, codecs, and software
  analytics are not fully settled by this general dictionary. For example,
  Skarnik gives [кадзіраванне](https://www.skarnik.by/rusbel/32158) and
  [дэкадзіраванне](https://www.skarnik.by/rusbel/17776), but the absence of a shorter
  form in one lookup is insufficient to declare every existing software variant
  wrong. No speculative wholesale rewrite of diagnostics was made.

The edited JSON was parsed successfully. A comparison with the starting catalogs
confirmed that retained entries preserve every placeholder and plural-category
set. Core and widget catalogs were unchanged in the initial September 27 pass. Runtime, layout, and target-device
verification are separate from this language review.

## Follow-up corrections — 2026-09-28

User review identified English `Plex Media Servers` inside Belarusian sentences,
which the initial pass had missed. Five deletion/privacy entries now use the
existing interface term **сервер Plex** with Belarusian case agreement, including
**на вашы серверы Plex** and **з абранымі вамі серверамі Plex**. The exact product
name **Plex Media Server** remains in the trademark notice. These are contextual
localization corrections, not additional dictionary findings.

The contribution action now uses the user-requested **Дапамажыце з перакладам**,
with the GitHub destination named in its supporting text. The former **унёсак**
is supported by [Skarnik's figurative contribution sense](https://www.skarnik.by/rusbel/9136);
the action was changed for clarity, not because the noun was incorrect.

The supplied language review also prefers **Змены набудуць сілу** and
**каб гэтая змена набыла сілу** in Language settings. The report-sending verb is
now consistently **дасылаць** across questions, answers, disclosures, previews,
and privacy text. One-shot diagnostic upload actions use **Даслаць дыягностыку**;
four widget entries were updated in this follow-up. Related forms include
**даслана**, **дасыланне**, and the neuter agreement **дасыланне было ўключана** /
**пры яго выключэнні**. Skarnik also records [даслаць](https://www.skarnik.by/belrus/26517)
and [дасланы](https://www.skarnik.by/belrus/26516). These confirm word forms,
while the supplied review determines the interface wording. **Справаздачы** is retained.
