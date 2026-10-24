//! Client-side sort and filter over stored rows, and the title comparator (plan §1.3, §1.10).
//! No request is made on a Sort & Filter change.
use super::model::{Kind, WatchRow};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// `titleSort` else `title`, via [`compare_titles`].
    Title,
    /// Server order is newest-first; ascending is its exact reverse (never `watchlistedAt:asc`).
    DateAdded,
    /// `originallyAvailableAt`; a missing date sorts last in both directions.
    Release,
    /// A missing rating sorts last in both directions.
    Rating,
}

impl SortKey {
    /// The fact this key orders by, named once for every page ([`crate::sortfacts`]).
    pub fn fact(self) -> crate::sortfacts::SortFact {
        use crate::sortfacts::SortFact;
        match self {
            SortKey::Title => SortFact::Title,
            SortKey::DateAdded => SortFact::DateAdded,
            SortKey::Release => SortFact::ReleaseDate,
            SortKey::Rating => SortFact::Rating,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortSpec {
    pub key: SortKey,
    pub desc: bool,
}

/// The "Show" filter (plan INPUT 1): `Released` keeps rows in the API released list;
/// `OnYourServers` keeps rows playable on a Reachable server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Show {
    #[default]
    All,
    Released,
    OnYourServers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FilterSpec {
    pub kind: Option<Kind>,
    pub show: Show,
}

/// Title order: Unicode-lowercase, fold only marks that are not letters in any shipped language
/// (e-acute to e, a-grave to a, c-cedilla to c, oe-ligature to oe); n-tilde, short-u (Be) and
/// short-i (Be) stay distinct (after n, u, i); Be yo and dotted-i take their alphabet slots (after е, after з); then the unfolded string, then the hex. No new
/// dependency. Operates on `titleSort` else `title`.
pub fn compare_titles(a: &WatchRow, b: &WatchRow) -> Ordering {
    let (ta, tb) = (sort_text(a), sort_text(b));
    fold_key(ta)
        .cmp(&fold_key(tb))
        .then_with(|| ta.cmp(tb))
        .then_with(|| a.hex.cmp(&b.hex))
}

fn sort_text(r: &WatchRow) -> &str {
    if r.title_sort.is_empty() {
        &r.title
    } else {
        &r.title_sort
    }
}

/// Collation key: lowercase, accent marks folded, and the three distinct letters given a slot
/// strictly between their base letter and the next one (every letter is scaled by 4 so the gap
/// exists): n < n-tilde < o, u < short-u < f, i < short-i < k (Cyrillic ones likewise).
fn fold_key(s: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            // Combining tilde / breve after the base letter make the distinct letter.
            '\u{303}' if out.last() == Some(&base('n')) => *out.last_mut().unwrap() += 2,
            '\u{306}' if out.last() == Some(&base('у')) || out.last() == Some(&base('и')) => {
                *out.last_mut().unwrap() += 2
            }
            // Combining diaeresis after e makes yo.
            '\u{308}' if out.last() == Some(&base('е')) => *out.last_mut().unwrap() += 2,
            '\u{300}'..='\u{36f}' => {}
            'ñ' => out.push(base('n') + 2),
            'ў' => out.push(base('у') + 2),
            'й' => out.push(base('и') + 2),
            'ё' => out.push(base('е') + 2),
            'і' => out.push(base('з') + 2),
            'œ' => out.extend([base('o'), base('e')]),
            'æ' => out.extend([base('a'), base('e')]),
            _ => out.push(base(fold_accent(c))),
        }
    }
    out
}

fn base(c: char) -> u32 {
    c as u32 * 4
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

/// Indices into `rows`, filtered then sorted. `available` answers the Show filter from the
/// committed tile state (row index to "playable on a Reachable server"); it is computed at commit,
/// never per frame.
pub fn sorted_filtered(rows: &[WatchRow], sort: &SortSpec, filter: &FilterSpec, available: &dyn Fn(usize) -> bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len())
        .filter(|&i| filter.kind.is_none_or(|k| rows[i].kind == k))
        .filter(|&i| match filter.show {
            Show::All => true,
            Show::Released => rows[i].released,
            Show::OnYourServers => available(i),
        })
        .collect();
    // A missing field sorts last whichever way the key runs, so the direction applies only to
    // present values.
    fn present<T: PartialOrd>(a: Option<&T>, b: Option<&T>, desc: bool) -> Ordering {
        match (a, b) {
            (Some(x), Some(y)) => {
                let o = x.partial_cmp(y).unwrap_or(Ordering::Equal);
                if desc { o.reverse() } else { o }
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
    idx.sort_by(|&i, &j| {
        let (a, b) = (&rows[i], &rows[j]);
        match sort.key {
            SortKey::Title => {
                let o = compare_titles(a, b);
                if sort.desc { o.reverse() } else { o }
            }
            // Server order is newest-first: descending is as sent, ascending its exact reverse.
            SortKey::DateAdded => {
                if sort.desc { a.order.cmp(&b.order) } else { b.order.cmp(&a.order) }
            }
            SortKey::Release => present(a.release.as_ref(), b.release.as_ref(), sort.desc).then(a.order.cmp(&b.order)),
            SortKey::Rating => present(a.rating.as_ref(), b.rating.as_ref(), sort.desc).then(a.order.cmp(&b.order)),
        }
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str, order: u32) -> WatchRow {
        let mut hex = [0u8; 12];
        hex[11] = order as u8;
        WatchRow {
            guid: format!("plex://movie/{order}"),
            hex,
            kind: Kind::Movie,
            title: title.into(),
            title_sort: String::new(),
            year: None,
            added_at: None,
            release: None,
            rating: None,
            released: true,
            order,
            thumb: String::new(),
        }
    }

    fn titles(rows: &[WatchRow], idx: &[usize]) -> Vec<String> {
        idx.iter().map(|&i| rows[i].title.clone()).collect()
    }

    fn by_title(list: &[&str]) -> Vec<String> {
        let rows: Vec<WatchRow> = list.iter().enumerate().map(|(i, t)| row(t, i as u32)).collect();
        let s = SortSpec { key: SortKey::Title, desc: false };
        let idx = sorted_filtered(&rows, &s, &FilterSpec::default(), &|_| true);
        titles(&rows, &idx)
    }

    #[test]
    fn case_is_folded_and_ties_fall_to_unfolded_then_hex() {
        assert_eq!(by_title(&["the b", "The a"]), ["The a", "the b"]);
        let (a, b) = (row("the", 1), row("The", 2));
        // Same folded key: the unfolded string orders them ("T" < "t").
        assert_eq!(compare_titles(&b, &a), Ordering::Less);
        let (c, d) = (row("Same", 1), row("Same", 2));
        assert_eq!(compare_titles(&c, &d), Ordering::Less);
        assert_eq!(compare_titles(&c, &c), Ordering::Equal);
    }

    #[test]
    fn title_sort_wins_over_title() {
        let mut a = row("The Zebra", 0);
        a.title_sort = "Zebra".into();
        let b = row("Moon", 1);
        assert_eq!(compare_titles(&b, &a), Ordering::Less);
    }

    #[test]
    fn french_marks_fold_into_the_base_letter() {
        assert_eq!(by_title(&["Zoé", "Étoile", "Eau", "Cœur", "Coeur2", "Ça", "Cb"]), ["Ça", "Cb", "Cœur", "Coeur2", "Eau", "Étoile", "Zoé"]);
    }

    #[test]
    fn n_tilde_is_its_own_letter_after_n() {
        assert_eq!(by_title(&["Ñandú", "Nz", "Oso", "Na"]), ["Na", "Nz", "Ñandú", "Oso"]);
        // Composed and decomposed spellings agree.
        let a = row("N\u{303}a", 0);
        let b = row("Ña", 1);
        assert_eq!(fold_key(&a.title), fold_key(&b.title));
    }

    #[test]
    fn belarusian_short_letters_are_distinct() {
        // у < ў < ф and и < й < к.
        assert_eq!(by_title(&["фа", "ўа", "уя", "ка", "йа", "ия"]), ["ия", "йа", "ка", "уя", "ўа", "фа"]);
        assert_ne!(fold_key("ў"), fold_key("у"));
        assert_ne!(fold_key("й"), fold_key("и"));
        assert_eq!(fold_key("и\u{306}"), fold_key("й"));
    }

    fn rated(title: &str, order: u32, rating: Option<f32>, release: Option<&str>) -> WatchRow {
        let mut r = row(title, order);
        r.rating = rating;
        r.release = release.map(String::from);
        r
    }

    fn order_of(rows: &[WatchRow], sort: SortSpec) -> Vec<u32> {
        sorted_filtered(rows, &sort, &FilterSpec::default(), &|_| true).iter().map(|&i| rows[i].order).collect()
    }

    #[test]
    fn date_added_ascending_is_the_exact_reverse_of_server_order() {
        let rows: Vec<WatchRow> = (0..4).map(|i| row("x", i)).collect();
        assert_eq!(order_of(&rows, SortSpec { key: SortKey::DateAdded, desc: true }), [0, 1, 2, 3]);
        assert_eq!(order_of(&rows, SortSpec { key: SortKey::DateAdded, desc: false }), [3, 2, 1, 0]);
    }

    #[test]
    fn missing_rating_and_release_sort_last_both_ways() {
        let rows = vec![
            rated("a", 0, None, None),
            rated("b", 1, Some(7.0), Some("2020-01-01")),
            rated("c", 2, Some(9.0), Some("2021-05-05")),
        ];
        for desc in [false, true] {
            let r = order_of(&rows, SortSpec { key: SortKey::Rating, desc });
            assert_eq!(*r.last().unwrap(), 0);
            let r = order_of(&rows, SortSpec { key: SortKey::Release, desc });
            assert_eq!(*r.last().unwrap(), 0);
        }
        assert_eq!(order_of(&rows, SortSpec { key: SortKey::Rating, desc: true }), [2, 1, 0]);
        assert_eq!(order_of(&rows, SortSpec { key: SortKey::Release, desc: false }), [1, 2, 0]);
    }

    #[test]
    fn title_descending_reverses_and_filters_apply_first() {
        let mut rows = vec![row("a", 0), row("b", 1), row("c", 2)];
        rows[1].kind = Kind::Show;
        let desc = SortSpec { key: SortKey::Title, desc: true };
        assert_eq!(order_of(&rows, desc), [2, 1, 0]);
        let movies = FilterSpec { kind: Some(Kind::Movie), show: Show::All };
        assert_eq!(sorted_filtered(&rows, &desc, &movies, &|_| true), [2, 0]);
        let avail = |i: usize| i != 2;
        let on = FilterSpec { kind: None, show: Show::OnYourServers };
        assert_eq!(sorted_filtered(&rows, &desc, &on, &avail), [1, 0]);
        let off = FilterSpec { kind: None, show: Show::Released };
        assert_eq!(sorted_filtered(&rows, &desc, &off, &avail), [2, 1, 0]);
    }

    #[test]
    fn show_released_drops_coming_soon_and_on_your_servers_drops_unknown() {
        let mut rows = vec![row("a", 0), row("b", 1), row("c", 2)];
        rows[1].released = false;
        let sort = SortSpec { key: SortKey::DateAdded, desc: true };
        let released = FilterSpec { kind: None, show: Show::Released };
        assert_eq!(sorted_filtered(&rows, &sort, &released, &|_| false), [0, 2]);
        // Row 2 stands for Unknown (not playable on a Reachable server).
        let held = FilterSpec { kind: None, show: Show::OnYourServers };
        assert_eq!(sorted_filtered(&rows, &sort, &held, &|i| i != 2), [0, 1]);
    }

    #[test]
    fn belarusian_yo_and_dotted_i_sort_at_their_alphabet_slots() {
        let rows: Vec<WatchRow> =
            ["Яблык", "Іван", "Ёлка", "Жук", "Зіма"].iter().enumerate().map(|(i, t)| row(t, i as u32)).collect();
        let sort = SortSpec { key: SortKey::Title, desc: false };
        let titles: Vec<&str> = sorted_filtered(&rows, &sort, &FilterSpec::default(), &|_| true)
            .iter()
            .map(|&i| rows[i].title.as_str())
            .collect();
        assert_eq!(titles, ["Ёлка", "Жук", "Зіма", "Іван", "Яблык"]);
    }
}
