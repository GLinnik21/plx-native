//! The bands a value sort's rail shows, and how a probe's COUNTS become rail offsets. Pure data and
//! functions: no read, no clock, no time zone, no global. The data job asks the counts of the
//! families [`BandFamily::rail_ready`] names (`listing_jobs.rs`, `letters.rs`) and hands the screens a
//! [`BandRail`]. A calendar family's job also gets `today` on the ask, the viewer's [`Zone`] from the
//! runner and the oldest year from one extent read ([`BandFamily::extent_read`]); the screens draw
//! all eight families' bands (`screens/src/library/rail_bands.rs` labels every [`Mark`]).
//!
//! The A-Z rail jumps to a letter by the running sum of per-letter counts. PMS has no per-band
//! directory for a VALUE sort (`/year` and `/decade` carry no `size`), so a band's offset comes from
//! one count-only probe per band EDGE instead: `GET .../all?<grid's own terms>&<term>` with
//! `Container-Start: 0` and `Container-Size: 0` answers `totalSize` = the number of items at or above
//! the edge `E`. [`BandFamily::of_sort`] says which sorts have bands, [`plan`] lists a family's bands
//! (highest first, each with its lower [`Edge`]), [`probes`] renders the probe of every edge and
//! [`offsets`] turns the counts that came back into each band's start and size in the listing's own
//! order. `docs/pms-api.md`, "Count probes at band edges", is the measurement behind every rule below
//! (PMS 1.43.5, one small library of each kind: "exact there, not proven at scale").
//!
//! Measured rules this encodes:
//!
//! - **A number probe is `<field>>>=<E minus one unit>`** (strictly greater, so "at or above `E`"): a
//!   rating edge minus 0.01 (floats are compared as floats, `rating>>=9.49` is every rating from 9.5
//!   up), a duration edge minus 1 ms, a count edge minus 1. A rating is held in integer hundredths
//!   here and never in an `f64`, so the text is exact.
//! - **A date probe is `<field>>=<E>`**: the inclusive operator with the exact epoch of the edge's
//!   first moment (accepted and exact on date fields, checked at one value per family). It keeps the
//!   1970s edge `>=0` and not `>>=-1`. A bare negative number on a date field is an ABSOLUTE epoch,
//!   not a relative date (`-3w` is one, `-315619200` is not; measured on `originallyAvailableAt` and
//!   `addedAt`), so the decades before 1970 are probed as `originallyAvailableAt>=-315619200` and the
//!   Release ladder reaches the oldest item's decade. What no measurement saw is a comparison at a
//!   real pre-1970 item (neither library held one). Were a server to clamp such an edge to zero, the
//!   bands before 1970 would come out empty and those items would fall into the unlabelled region,
//!   which is contiguous at the low end in both directions: never under a wrong label.
//! - **Which clock a date edge is read on** is the family's ([`Basis`]). `addedAt`, `lastViewedAt` and
//!   `episode.addedAt` are instants and compare against LOCAL midnight: the CALLER turns the civil
//!   date into an epoch (a converter taking the [`Day`], once per edge, because daylight saving is
//!   per edge), so the model never reads a zone. `originallyAvailableAt` is a date and compares as the
//!   epoch of UTC midnight, which needs no zone: the model computes it itself, so a release date
//!   before the 32-bit `time_t` range cannot take a probe away. A converter that fails (a 32-bit
//!   `time_t` cannot hold every local date) leaves that band without a probe, and the caller shows no
//!   rail.
//! - **Items with NO value** (unrated, never viewed, no release date) sort FIRST in `:asc` and LAST in
//!   `:desc`, and `>>=` excludes them. So do items below the lowest edge of a ladder that has no
//!   tail band (zero unplayed, zero plays). Both are the low end of the value order: an unlabelled
//!   [`Offsets::tail`] in a descending listing and an unlabelled [`Offsets::lead`] in an ascending
//!   one, belonging to no band.
//! - **A stored zero is not a value** to the rail. The listing shows a stored zero as nothing, and a
//!   probe at zero tells them apart: `>>=0` leaves out the no-value items AND the stored zeros, `>=0`
//!   counts the stored zeros. A tail band (`<75%`, `<1h`) is therefore probed as `>>=0`
//!   ([`Edge::Floor`]), so a 0.0 rating or a zero duration joins the unlabelled region, as the line
//!   under a poster shows it as a dash.
//! - **Offsets.** In `:desc` the first item below `E` sits at `count(>=E)`; in `:asc` the first item at
//!   or above `E` sits at `total - count(>=E)`. The two are mirror images of one another.
//! - **`episode.addedAt` is not mirror-symmetric**: `:desc` orders by the NEWEST episode, `:asc` by the
//!   OLDEST, so [`BandFamily::LastEpisodeAdded`] has bands for a descending sort only.
//! - **A filter can be silently IGNORED** (a wrong key answers the whole library, no error). A count
//!   sequence that is all equal to the total, is not monotone, or exceeds the total is therefore
//!   [`Reject`]ed, and the rail stays hidden as it does today.
//!
//! Not exercised by any measurement: a stored rating of 0.0 or duration of 0 (none existed), a
//! comparison at a real pre-1970 item, and `>=` at more than one value per family.

use super::{LibraryType, SecKind};

/// A civil date as the application's clock hands it out: `[year, month, day]`, month `1..=12`.
pub type Day = [i32; 3];

/// The first year the model draws a band for. The extent (the oldest year) comes off the server, and a
/// bogus year (`0`, `1`, a corrupt value) must not become two thousand bands; no real item is older.
pub const EARLIEST_YEAR: i32 = 1000;

/// The ladder of a Critic rating (`95% 90% 85% 80% 75% <75%`): the percent a band starts at, which is
/// also the edge in tenths of a point (95 = a rating of 9.5).
const RATING_PERCENTS: [u8; 5] = [95, 90, 85, 80, 75];
/// The ladder of a duration (`3h+ 2½h 2h 1½h 1h <1h`), in minutes.
const DURATION_MINUTES: [u16; 5] = [180, 150, 120, 90, 60];
/// The ladder of an unplayed count (`12+ 10 8 6 4 2 1`). A show with none left has no band.
const UNPLAYED_COUNTS: [u32; 7] = [12, 10, 8, 6, 4, 2, 1];
/// The ladder of a play count (`10+ 5 3 2 1`). An item never played has no band.
const PLAYS_COUNTS: [u32; 5] = [10, 5, 3, 2, 1];

/// What an edge's civil date is measured against on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Basis {
    /// An instant (`addedAt`, `lastViewedAt`, `episode.addedAt`): the viewer's LOCAL midnight.
    Local,
    /// A date (`originallyAvailableAt`): the epoch of UTC midnight, whatever zone the viewer is in.
    Utc,
}

/// The viewer's time zone as the two reads of the OS a calendar family needs, handed to a bands job
/// by the application's runner as plain functions (`plx_platform::i18n::local_midnight` and
/// `local_calendar_date`): the model and the job body read no zone and no clock themselves, and a test
/// passes a fake. A civil date to its first moment (asked once per [`Basis::Local`] edge, daylight
/// saving being per date), and an instant to the civil date it falls on; `None` from either where the
/// OS cannot say.
#[derive(Clone, Copy, Debug)]
pub struct Zone {
    /// The epoch of the viewer's local midnight at the start of a civil day.
    pub local_midnight: fn(Day) -> Option<i64>,
    /// The civil date, in the viewer's zone, of an epoch.
    pub local_date: fn(i64) -> Option<Day>,
}

/// A viewer on UTC, for the tests that run the production body: local midnight is UTC midnight and a
/// local date the UTC one (Howard Hinnant's `civil_from_days`).
#[cfg(test)]
pub(super) fn utc_zone() -> Zone {
    fn civil(epoch: i64) -> Option<Day> {
        let z = epoch.div_euclid(86_400) + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Some([i32::try_from(year).ok()?, i32::try_from(month).ok()?, i32::try_from(day).ok()?])
    }
    Zone { local_midnight: |date| Some(utc_midnight(date)), local_date: civil }
}

/// The first year a calendar family draws a band for, whatever its extent says. An extent comes off the
/// server, and a year before this one (an 1895 film, a bogus `0001-01-01`) would mean a probe for every
/// year since: [`extent_year`] stops at it, so a rail is at most `current year - 1900 + 13` probes.
/// The items older than it sit below the lowest edge, in the unlabelled region at the low end of the
/// value order in both directions, never under a wrong label.
pub const FIRST_PLAUSIBLE_YEAR: i32 = 1900;

/// The year [`plan`] starts a calendar family's bands from, given the year of the oldest item on
/// `today`: the extent itself, no earlier than [`FIRST_PLAUSIBLE_YEAR`]. `None` for an extent AFTER
/// this year, which no item can have (nothing is older than the present), so the family has no rail.
pub fn extent_year(year: i32, today: Day) -> Option<i32> {
    (year <= today[0]).then(|| year.max(FIRST_PLAUSIBLE_YEAR))
}

/// How a calendar family finds its oldest value ([`BandFamily::extent_read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtentRead {
    /// The date field of a listed item, sorted `:asc:nullsLast` for the first row.
    pub field: &'static str,
    /// Read it from the section's EPISODES (`type=4`, unfiltered) instead of the page's own listing: a
    /// show row carries no `episode.addedAt`, and the oldest episode is not newer than any show's newest
    /// one, so the year it gives never cuts a band off. It may leave empty years at the old end.
    pub episodes: bool,
}

/// The sorts that have bands, one per board table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BandFamily {
    /// Critic rating (`rating`).
    Rating,
    /// Duration (`duration`, milliseconds on the wire).
    Duration,
    /// Date added (`addedAt`).
    DateAdded,
    /// Date viewed (`lastViewedAt`); an item never viewed has no band.
    DateViewed,
    /// Last episode added (`episode.addedAt`), a show sort; descending only.
    LastEpisodeAdded,
    /// Unplayed episodes (`unviewedLeafCount`), a show sort.
    Unplayed,
    /// Plays (`viewCount`), the sort the application adds itself.
    Plays,
    /// Release date (`originallyAvailableAt`), banded by year and decade.
    Release,
}

impl BandFamily {
    /// Every family.
    pub const ALL: [BandFamily; 8] = [
        Self::Rating,
        Self::Duration,
        Self::DateAdded,
        Self::DateViewed,
        Self::LastEpisodeAdded,
        Self::Unplayed,
        Self::Plays,
        Self::Release,
    ];

    /// The family a sort has bands in, or `None` (the rail stays hidden, or is the letters'): `key` is
    /// the sort entry's key as the server lists it (`SortEntry::key`), `descending` the direction the
    /// page asks, `listing` the type the grid lists. A section's own listing
    /// ([`LibraryType::Primary`]) has the bands of the films' and the shows' tables, and a show
    /// library's EPISODES listing has four of them: date added, date viewed, release date and
    /// rating (`docs/pms-api.md`, "Count probes for seasons and episodes"). Date added and release
    /// date were VERIFIED at their edges; the critic rating and the date viewed were accepted as
    /// filters but no episode had a value on the measured server, so neither was ever checked at an
    /// edge. They stay mapped, since a library that has values may be exact: a field nobody has a
    /// value in counts nothing (`bands_available` needs two populated bands: no rail), and a filter
    /// the server ignored or counts that are not monotone are refused by the read (`count_bands`,
    /// `BandRail::new`). Its audience score is rounded by the server (no family counts it), its Title has the letters, and
    /// `unviewedLeafCount`, `duration`,
    /// `episode.addedAt` and the plays are not episode sorts. Seasons declare Title and Random alone,
    /// and Collections list one sort: no bands. The WHOLE key is read, never its table-stripped
    /// tail: `episode.addedAt` is another family than `addedAt`. A key of the other library kind has
    /// no band (`duration` is not a show field, `unviewedLeafCount` and `episode.addedAt` are not
    /// movie fields: the server would silently ignore the filter). `rating` is offered for both
    /// kinds, as the server lists it for both (a show with no rating has all its items in the
    /// unlabelled region).
    pub fn of_sort(key: &str, descending: bool, kind: SecKind, listing: LibraryType) -> Option<BandFamily> {
        // Exhaustive on purpose, like `kind_offers_plays_sort`: a third kind must decide here.
        let (movie, show) = match kind {
            SecKind::Movie => (true, false),
            SecKind::Show => (false, true),
        };
        // Exhaustive on purpose too: a fifth type must decide here.
        match listing {
            LibraryType::Primary => {}
            // Episodes exist in a show library only (`LibraryType::offered`); the sorts below are the
            // four of the episode menu that have a family, and nothing else of it does.
            LibraryType::Episodes => {
                return match key {
                    "rating" => show.then_some(Self::Rating),
                    "addedAt" => show.then_some(Self::DateAdded),
                    "lastViewedAt" => show.then_some(Self::DateViewed),
                    "originallyAvailableAt" => show.then_some(Self::Release),
                    _ => None,
                };
            }
            LibraryType::Seasons | LibraryType::Collections => return None,
        }
        match key {
            "rating" => Some(Self::Rating),
            "duration" => movie.then_some(Self::Duration),
            "addedAt" => Some(Self::DateAdded),
            "lastViewedAt" => Some(Self::DateViewed),
            "episode.addedAt" => (show && descending).then_some(Self::LastEpisodeAdded),
            "unviewedLeafCount" => show.then_some(Self::Unplayed),
            super::PLAYS_SORT_KEY => Some(Self::Plays),
            "originallyAvailableAt" => Some(Self::Release),
            _ => None,
        }
    }

    /// The field a probe filters on: the sort's own key.
    pub fn field(self) -> &'static str {
        match self {
            Self::Rating => "rating",
            Self::Duration => "duration",
            Self::DateAdded => "addedAt",
            Self::DateViewed => "lastViewedAt",
            Self::LastEpisodeAdded => "episode.addedAt",
            Self::Unplayed => "unviewedLeafCount",
            Self::Plays => super::PLAYS_SORT_KEY,
            Self::Release => "originallyAvailableAt",
        }
    }

    /// Does the A-Z rail draw this family's bands? THE gate, and the only place to narrow or widen.
    /// Every family is drawn: the four whose edges are fixed numbers (rating, duration, unplayed,
    /// plays) need nothing from the viewer's clock or the server's extent, and the calendar ones (date
    /// added, date viewed, last episode added, release year) are counted with `today` on the ask, the
    /// [`Zone`] and the extent read, so a calendar family shows no rail until the application has
    /// handed the Library the day (`rail_family` in `letters.rs` asks for both). The match stays
    /// exhaustive: a ninth family must decide here.
    pub fn rail_ready(self) -> bool {
        match self {
            Self::Rating
            | Self::Duration
            | Self::Unplayed
            | Self::Plays
            | Self::DateAdded
            | Self::DateViewed
            | Self::LastEpisodeAdded
            | Self::Release => true,
        }
    }

    /// The bands of a family whose edges are fixed numbers, highest first, or `None` for a calendar
    /// family ([`Self::basis`]), whose bands need `today` and the oldest year ([`plan`]). A fixed
    /// ladder reads neither, so a caller with no clock gets its plan here instead of inventing a date.
    pub fn fixed_ladder(self) -> Option<Vec<Band>> {
        self.basis().is_none().then(|| plan(self, [EARLIEST_YEAR, 1, 1], EARLIEST_YEAR))
    }

    /// Does a watch move this family's counts? A play count and an unplayed count change with every
    /// title watched, as a date viewed does; a rating, a duration and a date added do not, and
    /// neither does the newest episode of a show.
    pub fn moves_with_viewing(self) -> bool {
        // Exhaustive on purpose, like `rail_ready`.
        match self {
            Self::Plays | Self::Unplayed | Self::DateViewed => true,
            Self::Rating | Self::Duration | Self::DateAdded | Self::LastEpisodeAdded | Self::Release => false,
        }
    }

    /// How the family finds the oldest value for [`plan`]'s `oldest_year`, or `None` for a family with
    /// fixed edges. `Some` exactly when [`Self::basis`] is: the dates are the ones that need an extent.
    pub fn extent_read(self) -> Option<ExtentRead> {
        let (field, episodes) = match self {
            Self::DateAdded => ("addedAt", false),
            Self::DateViewed => ("lastViewedAt", false),
            Self::LastEpisodeAdded => ("addedAt", true),
            Self::Release => ("originallyAvailableAt", false),
            Self::Rating | Self::Duration | Self::Unplayed | Self::Plays => return None,
        };
        Some(ExtentRead { field, episodes })
    }

    /// What the family's civil-date edges are measured against, or `None` for a family whose edges
    /// are plain numbers. `Some` also means [`plan`] reads `today` and the oldest year.
    pub fn basis(self) -> Option<Basis> {
        match self {
            Self::DateAdded | Self::DateViewed | Self::LastEpisodeAdded => Some(Basis::Local),
            Self::Release => Some(Basis::Utc),
            Self::Rating | Self::Duration | Self::Unplayed | Self::Plays => None,
        }
    }
}

/// A band's lower edge, typed by what it counts. Derived `Ord` is the order of values within ONE
/// plan (a plan never mixes variants, except that [`Edge::Floor`] is the lowest of all), so a plan
/// from highest to lowest is strictly decreasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Edge {
    /// Every item with a value ABOVE ZERO: the edge of a tail band (`<75%`, `<1h`), probed as `>>=0`.
    /// It leaves out the no-value items and the stored zeros alike.
    Floor,
    /// A critic rating, in tenths of a point: 95 is a rating of 9.5.
    Rating(u8),
    /// A duration in minutes.
    Minutes(u16),
    /// A count of episodes or plays.
    Count(u32),
    /// The first moment of a civil day, read on the clock the basis names.
    Day { date: Day, basis: Basis },
}

/// What a band is called: the identity a focus key hangs on and the token the screens layer turns
/// into a localised label. Unique within a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// The current day, a band of its own.
    Today,
    /// A month of the current year, `1..=12`.
    Month(u8),
    /// A calendar year.
    Year(i32),
    /// A decade by its first year: 2020 is "2020s".
    Decade(i32),
    /// `95%`.
    Percent(u8),
    /// `<75%`: everything rated, below the lowest percent.
    BelowPercent(u8),
    /// `2½h`.
    Minutes(u16),
    /// `3h+`: the top of a duration ladder.
    MinutesOrMore(u16),
    /// `<1h`: everything with a duration, below the lowest.
    BelowMinutes(u16),
    /// `10`: at least this many.
    Count(u32),
    /// `12+`: the top of a count ladder.
    CountOrMore(u32),
}

/// One band of a rail: its family, its label token and the lower edge of the values it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Band {
    family: BandFamily,
    mark: Mark,
    edge: Edge,
}

impl Band {
    fn new(family: BandFamily, mark: Mark, edge: Edge) -> Band {
        Band { family, mark, edge }
    }

    /// The family the band belongs to.
    pub fn family(&self) -> BandFamily {
        self.family
    }

    /// The label token (and focus identity).
    pub fn mark(&self) -> Mark {
        self.mark
    }

    /// The lower edge: the band holds the values at or above it, down to the next band's edge.
    pub fn edge(&self) -> Edge {
        self.edge
    }

    /// The count-only probe whose answer is the number of items at or above this band's edge.
    /// `local_midnight` turns a civil day into the epoch of the viewer's LOCAL midnight (the
    /// caller's, asked once per [`Basis::Local`] edge and never for a UTC one); `None` from it
    /// leaves the band without a probe.
    pub fn probe(&self, local_midnight: &dyn Fn(Day) -> Option<i64>) -> Option<Probe> {
        let (op, value) = match self.edge {
            Edge::Floor => (AFTER, "0".to_owned()),
            Edge::Rating(tenths) => (AFTER, hundredths(i32::from(tenths) * 10 - 1)),
            Edge::Minutes(minutes) => (AFTER, (i64::from(minutes) * 60_000 - 1).to_string()),
            Edge::Count(count) => (AFTER, (i64::from(count) - 1).to_string()),
            Edge::Day { date, basis } => {
                let epoch = match basis {
                    Basis::Local => local_midnight(date)?,
                    Basis::Utc => utc_midnight(date),
                };
                (AT_LEAST, epoch.to_string())
            }
        };
        Some(Probe { field: self.family.field(), op, value })
    }
}

/// The strictly-greater operator `>>=`, minus the `=` the query string supplies: the number probes'.
const AFTER: &str = ">>";
/// The inclusive operator `>=`, minus the `=` the query string supplies: the date probes'.
const AT_LEAST: &str = ">";

/// Days from 1970-01-01 to `date` in the proleptic Gregorian calendar (Howard Hinnant's
/// `days_from_civil`): negative before 1970. Pure arithmetic, no zone and no `time_t`.
fn days_from_civil(date: Day) -> i64 {
    let [year, month, day] = date.map(i64::from);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The epoch second of `date`'s first moment in UTC: what `originallyAvailableAt` compares against.
fn utc_midnight(date: Day) -> i64 {
    days_from_civil(date) * 86_400
}

/// A rating of `h` hundredths as the decimal text the server compares as a float: 949 is `9.49`.
fn hundredths(h: i32) -> String {
    let sign = if h < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", h.abs() / 100, h.abs() % 100)
}

/// One term of a count-only read: the field, the operator and the value. The operator is held
/// WITHOUT its trailing `=`, which is the query string's own (`>>=` is the key `rating>>` and `=`):
/// the request builder writes `{key}={value}`, so [`Probe::query_pair`] is what it takes. The key is
/// NOT percent-encoded here (`>>` needs `%3E%3E`); that stays the request builder's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The sort key being filtered (`episode.addedAt` keeps its scope).
    pub field: &'static str,
    /// `>>` for the strictly-greater `>>=` of the number probes, `>` for the inclusive `>=` of the
    /// date probes.
    pub op: &'static str,
    /// The decimal value after the `=`.
    pub value: String,
}

impl Probe {
    /// The key and the value of the query pair: `("rating>>", "9.49")`, `("addedAt>", "1793422800")`.
    pub fn query_pair(&self) -> (String, &str) {
        (format!("{}{}", self.field, self.op), &self.value)
    }

    /// The term as one string: `rating>>=9.49`.
    pub fn term(&self) -> String {
        let (key, value) = self.query_pair();
        format!("{key}={value}")
    }
}

/// The probe of every band, in the plan's order, or `None` if any band has none (all or nothing:
/// a rail built from some of its counts would point at the wrong rows). `local_midnight` is
/// [`Band::probe`]'s.
pub fn probes(bands: &[Band], local_midnight: &dyn Fn(Day) -> Option<i64>) -> Option<Vec<Probe>> {
    bands.iter().map(|band| band.probe(local_midnight)).collect()
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: i32) -> Option<i32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if is_leap(year) { 29 } else { 28 }),
        _ => None,
    }
}

/// Is `day` a real date of the calendar the model draws (a year from [`EARLIEST_YEAR`])?
fn is_valid(day: Day) -> bool {
    let [year, month, date] = day;
    year >= EARLIEST_YEAR && days_in_month(year, month).is_some_and(|length| (1..=length).contains(&date))
}

/// The bands `family` shows, highest first: the order the rail reads in a descending listing, and
/// the order [`probes`] and [`offsets`] take their counts in. `today` and `oldest_year` (the year of
/// the oldest item, from an extent read) matter only to a family with a [`BandFamily::basis`]; a
/// `today` that is not a real date gives no bands. `oldest_year` is read on the family's own clock:
/// the LOCAL year of the oldest item for a [`Basis::Local`] family (the year its local date falls
/// in), the release year for [`BandFamily::Release`].
///
/// - Rating, duration, unplayed and plays are fixed ladders (see the constants). Rating and duration
///   end in a tail band for the lowest values; unplayed and plays end at their lowest count, and an
///   item below it has no band.
/// - Date added, date viewed and last episode added: `Today`, then the months of the current year
///   back to January, then every calendar year back to `oldest_year`. Today is a band of its own and
///   yesterday falls in its month. The current month is skipped when today is its first (no day of
///   it is before today), so on 1 January there is `Today` and then years.
/// - Release date: the current year alone, then decades for everything older (2026, 2020s, 2010s,
///   ...); the current decade is skipped when the current year opens it.
///
/// No band is drawn for a year before [`EARLIEST_YEAR`].
pub fn plan(family: BandFamily, today: Day, oldest_year: i32) -> Vec<Band> {
    match family {
        BandFamily::Rating => {
            let mut bands: Vec<Band> = RATING_PERCENTS
                .iter()
                .map(|&percent| Band::new(family, Mark::Percent(percent), Edge::Rating(percent)))
                .collect();
            let lowest = RATING_PERCENTS[RATING_PERCENTS.len() - 1];
            bands.push(Band::new(family, Mark::BelowPercent(lowest), Edge::Floor));
            bands
        }
        BandFamily::Duration => {
            let top = DURATION_MINUTES[0];
            let mut bands: Vec<Band> = DURATION_MINUTES
                .iter()
                .map(|&minutes| {
                    let mark = if minutes == top { Mark::MinutesOrMore(minutes) } else { Mark::Minutes(minutes) };
                    Band::new(family, mark, Edge::Minutes(minutes))
                })
                .collect();
            let lowest = DURATION_MINUTES[DURATION_MINUTES.len() - 1];
            bands.push(Band::new(family, Mark::BelowMinutes(lowest), Edge::Floor));
            bands
        }
        BandFamily::Unplayed => counts(family, &UNPLAYED_COUNTS),
        BandFamily::Plays => counts(family, &PLAYS_COUNTS),
        BandFamily::DateAdded | BandFamily::DateViewed | BandFamily::LastEpisodeAdded => {
            calendar(family, Basis::Local, today, oldest_year)
        }
        BandFamily::Release => release(family, today, oldest_year),
    }
}

/// A ladder of counts: the first rung is open at the top (`12+`).
fn counts(family: BandFamily, rungs: &[u32]) -> Vec<Band> {
    rungs
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            let mark = if i == 0 { Mark::CountOrMore(count) } else { Mark::Count(count) };
            Band::new(family, mark, Edge::Count(count))
        })
        .collect()
}

fn calendar(family: BandFamily, basis: Basis, today: Day, oldest_year: i32) -> Vec<Band> {
    let Ok(month) = u8::try_from(today[1]) else { return Vec::new() };
    if !is_valid(today) {
        return Vec::new();
    }
    let [year, _, date] = today;
    let day = |date: Day| Edge::Day { date, basis };
    let mut bands = vec![Band::new(family, Mark::Today, day(today))];
    // The current month holds the days before today: none when today is its first.
    let newest_month = if date == 1 { month - 1 } else { month };
    for month in (1..=newest_month).rev() {
        bands.push(Band::new(family, Mark::Month(month), day([year, i32::from(month), 1])));
    }
    for older in (oldest_year.max(EARLIEST_YEAR)..year).rev() {
        bands.push(Band::new(family, Mark::Year(older), day([older, 1, 1])));
    }
    bands
}

fn release(family: BandFamily, today: Day, oldest_year: i32) -> Vec<Band> {
    if !is_valid(today) {
        return Vec::new();
    }
    let year = today[0];
    let day = |date: Day| Edge::Day { date, basis: Basis::Utc };
    let mut bands = vec![Band::new(family, Mark::Year(year), day([year, 1, 1]))];
    // The current decade holds the years before this one: none when this year opens it.
    let oldest_year = oldest_year.max(EARLIEST_YEAR);
    if oldest_year < year {
        let this_decade = year.div_euclid(10) * 10;
        let mut decade = if year == this_decade { this_decade - 10 } else { this_decade };
        while decade >= oldest_year.div_euclid(10) * 10 {
            bands.push(Band::new(family, Mark::Decade(decade), day([decade, 1, 1])));
            decade -= 10;
        }
    }
    bands
}

/// Where one band sits in the listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    /// The band.
    pub band: Band,
    /// The index of its first item in the listing. An empty band starts where the next one does.
    pub start: u32,
    /// How many items it holds.
    pub size: u32,
}

/// A rail's layout over one listing, in the listing's own order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offsets {
    /// Items before the first band that belong to none: the no-value items of an ascending listing.
    pub lead: u32,
    /// The bands: highest first for a descending listing, lowest first for an ascending one.
    pub bands: Vec<Start>,
    /// Items after the last band that belong to none: the no-value items of a descending listing.
    pub tail: u32,
}

impl Offsets {
    /// How many bands hold at least one item (a rail with fewer than two is not worth showing).
    pub fn populated(&self) -> usize {
        self.bands.iter().filter(|start| start.size > 0).count()
    }
}

/// Why a count sequence is not trusted. Every one leaves the rail hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// There is no band to place.
    NoBands,
    /// The probes did not answer one count per band.
    WrongLength { bands: usize, counts: usize },
    /// A count is more than the listing holds: the counts are of some other listing.
    AboveTotal { index: usize },
    /// A count is below the one before it, though a lower edge admits every item a higher one does.
    NotMonotone { index: usize },
    /// Every count is the total: the server ignored the filter and answered the whole library.
    AllEqualTotal,
}

/// Turn the counts into each band's start and size.
///
/// `at_or_above[i]` is the answer of `bands[i]`'s probe, the number of items at or above its edge, with
/// `bands` in [`plan`]'s order (highest first). `total` is the listing's size, `descending` its
/// direction. A descending listing starts band `i` at `at_or_above[i - 1]`; an ascending one reads
/// the bands lowest first and starts band `i` at `total - at_or_above[i]`: the same intervals
/// mirrored. The items below the lowest edge are `total - at_or_above[last]`, and are the
/// [`Offsets::tail`] of a descending listing or the [`Offsets::lead`] of an ascending one.
///
/// A sequence is rejected (the checks run in this order) if the lengths differ, a count exceeds
/// `total`, the counts decrease along the plan, or all of them equal `total`.
pub fn offsets(bands: &[Band], at_or_above: &[u32], total: u32, descending: bool) -> Result<Offsets, Reject> {
    if bands.len() != at_or_above.len() {
        return Err(Reject::WrongLength { bands: bands.len(), counts: at_or_above.len() });
    }
    let Some(&lowest) = at_or_above.last() else { return Err(Reject::NoBands) };
    if let Some(index) = at_or_above.iter().position(|&count| count > total) {
        return Err(Reject::AboveTotal { index });
    }
    if let Some(index) = at_or_above.windows(2).position(|pair| pair[1] < pair[0]) {
        return Err(Reject::NotMonotone { index: index + 1 });
    }
    if at_or_above.iter().all(|&count| count == total) {
        return Err(Reject::AllEqualTotal);
    }
    let mut starts = Vec::with_capacity(bands.len());
    let mut above = 0;
    for (band, &count) in bands.iter().zip(at_or_above) {
        let start = if descending { above } else { total - count };
        starts.push(Start { band: *band, start, size: count - above });
        above = count;
    }
    if !descending {
        starts.reverse();
    }
    let unbanded = total - lowest;
    let (lead, tail) = if descending { (0, unbanded) } else { (unbanded, 0) };
    Ok(Offsets { lead, bands: starts, tail })
}

/// The counts of one listing's probes, turned into the rail's layout in BOTH directions once, when
/// they land. A value sort's counts are in value order and the direction is not part of what was
/// counted, so a flip of the sort's direction picks the other layout ([`Self::listed`]) and asks
/// the server nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BandRail {
    ascending: Offsets,
    descending: Offsets,
}

impl BandRail {
    /// The layouts for `at_or_above` counts of `bands` over a listing of `total` items, or why the
    /// counts are not trusted ([`offsets`]; a [`Reject`] does not depend on the direction).
    pub fn new(bands: &[Band], at_or_above: &[u32], total: u32) -> Result<BandRail, Reject> {
        Ok(BandRail {
            ascending: offsets(bands, at_or_above, total, false)?,
            descending: offsets(bands, at_or_above, total, true)?,
        })
    }

    /// The layout in the listing's own order, for the direction the page asks.
    pub fn listed(&self, descending: bool) -> &Offsets {
        if descending { &self.descending } else { &self.ascending }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const LOCAL_ZONE_SECONDS: i64 = 5 * 3600;

    fn days(date: Day) -> i64 {
        days_from_civil(date)
    }

    /// A viewer five hours behind UTC: local midnight is 05:00 UTC, so it differs from UTC midnight.
    fn fake_local(date: Day) -> Option<i64> {
        Some(utc_midnight(date) + LOCAL_ZONE_SECONDS)
    }

    /// The epoch a date edge is probed at: local midnight of the fake viewer, or UTC midnight.
    fn epoch_of(edge: Edge) -> i64 {
        match edge {
            Edge::Day { date, basis: Basis::Local } => fake_local(date).unwrap(),
            Edge::Day { date, basis: Basis::Utc } => utc_midnight(date),
            other => panic!("{other:?}"),
        }
    }

    fn terms(bands: &[Band]) -> Vec<String> {
        probes(bands, &fake_local).expect("every edge converts").iter().map(Probe::term).collect()
    }

    fn marks(bands: &[Band]) -> Vec<Mark> {
        bands.iter().map(Band::mark).collect()
    }

    #[test]
    fn the_calendar_agrees_with_known_epochs() {
        // Literals from an independent calendar (Python's `calendar.timegm`), not from this code.
        for (date, epoch) in [
            ([1970, 1, 1], 0),
            ([2026, 10, 31], 1_793_404_800),
            ([2026, 1, 1], 1_767_225_600),
            ([2020, 1, 1], 1_577_836_800),
            ([1990, 1, 1], 631_152_000),
            ([1969, 12, 31], -86_400),
            ([1960, 1, 1], -315_619_200),
            ([1900, 3, 1], -2_203_891_200),
            ([1000, 1, 1], -30_610_224_000),
            ([2000, 2, 29], 951_782_400),
            ([2038, 1, 19], 2_147_472_000),
            ([2100, 3, 1], 4_107_542_400),
        ] {
            assert_eq!(utc_midnight(date), epoch, "{date:?}");
        }
        assert_eq!(days([2024, 3, 1]) - days([2024, 2, 1]), 29);
    }

    // ---- of_sort ----

    #[test]
    fn of_sort_maps_the_applications_sort_keys() {
        use BandFamily::*;
        use SecKind::{Movie, Show};
        let row = |key: &str, desc: bool, kind: SecKind| BandFamily::of_sort(key, desc, kind, LibraryType::Primary);
        for desc in [false, true] {
            assert_eq!(row("rating", desc, Movie), Some(Rating));
            assert_eq!(row("rating", desc, Show), Some(Rating));
            assert_eq!(row("duration", desc, Movie), Some(Duration));
            assert_eq!(row("addedAt", desc, Movie), Some(DateAdded));
            assert_eq!(row("addedAt", desc, Show), Some(DateAdded));
            assert_eq!(row("lastViewedAt", desc, Movie), Some(DateViewed));
            assert_eq!(row("lastViewedAt", desc, Show), Some(DateViewed));
            assert_eq!(row("unviewedLeafCount", desc, Show), Some(Unplayed));
            assert_eq!(row("viewCount", desc, Movie), Some(Plays));
            assert_eq!(row("viewCount", desc, Show), Some(Plays));
            assert_eq!(row("originallyAvailableAt", desc, Movie), Some(Release));
            assert_eq!(row("originallyAvailableAt", desc, Show), Some(Release));
        }
        // Last episode added is descending only: ascending orders by the OLDEST episode.
        assert_eq!(row("episode.addedAt", true, Show), Some(LastEpisodeAdded));
        assert_eq!(row("episode.addedAt", false, Show), None);
        // A key of the other library kind would be silently ignored by the server.
        assert_eq!(row("duration", true, Show), None);
        assert_eq!(row("unviewedLeafCount", true, Movie), None);
        assert_eq!(row("episode.addedAt", true, Movie), None);
    }

    #[test]
    fn of_sort_leaves_every_other_sort_without_bands() {
        for kind in [SecKind::Movie, SecKind::Show] {
            for desc in [false, true] {
                for key in [
                    "titleSort",
                    "random",
                    "audienceRating",
                    "mediaHeight",
                    "userRating",
                    "year",
                    "show.titleSort,season.index",
                    // The whole key is read: a table prefix or a direction suffix is another key.
                    "show.addedAt",
                    "addedAt:desc",
                    "Rating",
                    "",
                ] {
                    assert_eq!(BandFamily::of_sort(key, desc, kind, LibraryType::Primary), None, "{key} {desc} {kind:?}");
                }
            }
        }
    }

    #[test]
    fn of_sort_knows_the_key_the_application_adds_itself() {
        let plays = BandFamily::of_sort(super::super::PLAYS_SORT_KEY, true, SecKind::Movie, LibraryType::Primary);
        assert_eq!(plays, Some(BandFamily::Plays));
        assert_eq!(BandFamily::Plays.field(), super::super::PLAYS_SORT_KEY);
    }

    /// **Which listing has which bands** (`docs/pms-api.md`, "Count probes for seasons and episodes"). The
    /// section's own listing has its tables'; the Episodes listing of a show library has the four sorts
    /// of its menu that have a family (date added and release date verified at their edges; date viewed
    /// and rating accepted as filters with no values observed, so no rail where nobody has one) and no other (its audience score is rounded, `unviewedLeafCount`, `duration`, `episode.addedAt` and the
    /// plays are not its sorts, its Title has the letters); Seasons (Title and Random alone) and
    /// Collections have none. Both directions, and the other library kind: Episodes exist for a show
    /// library alone.
    #[test]
    fn which_listing_has_which_bands() {
        use BandFamily::{DateAdded, DateViewed, Duration, LastEpisodeAdded, Plays, Rating, Release, Unplayed};
        use LibraryType::{Collections, Episodes, Primary, Seasons};
        use SecKind::{Movie, Show};
        let table: [(&str, SecKind, [Option<BandFamily>; 4]); 14] = [
            // key,                  kind,  [Primary,          Seasons, Episodes,          Collections]
            ("rating", Show, [Some(Rating), None, Some(Rating), None]),
            ("rating", Movie, [Some(Rating), None, None, None]),
            ("addedAt", Show, [Some(DateAdded), None, Some(DateAdded), None]),
            ("addedAt", Movie, [Some(DateAdded), None, None, None]),
            ("lastViewedAt", Show, [Some(DateViewed), None, Some(DateViewed), None]),
            ("originallyAvailableAt", Show, [Some(Release), None, Some(Release), None]),
            ("originallyAvailableAt", Movie, [Some(Release), None, None, None]),
            ("duration", Movie, [Some(Duration), None, None, None]),
            ("duration", Show, [None, None, None, None]),
            ("unviewedLeafCount", Show, [Some(Unplayed), None, None, None]),
            ("episode.addedAt", Show, [Some(LastEpisodeAdded), None, None, None]),
            ("viewCount", Show, [Some(Plays), None, None, None]),
            ("audienceRating", Show, [None, None, None, None]),
            ("titleSort", Show, [None, None, None, None]),
        ];
        for (key, kind, want) in table {
            for desc in [false, true] {
                for (listing, family) in [Primary, Seasons, Episodes, Collections].into_iter().zip(want) {
                    // the last-episode sort is descending only, in the one listing that has it
                    let family = if key == "episode.addedAt" && !desc { None } else { family };
                    assert_eq!(BandFamily::of_sort(key, desc, kind, listing), family, "{key} desc={desc} {kind:?} {listing:?}");
                }
            }
        }
        // the Episodes menu's Title entry (a composite) and Random: the letters' sort, and none
        for key in [
            "show.titleSort,season.index:nullsLast,episode.index:nullsLast,episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id",
            "random",
        ] {
            assert_eq!(BandFamily::of_sort(key, false, Show, Episodes), None, "{key}");
        }
    }

    #[test]
    fn a_family_filters_on_its_sorts_own_key() {
        for family in BandFamily::ALL {
            let kind = if matches!(family, BandFamily::Unplayed | BandFamily::LastEpisodeAdded) {
                SecKind::Show
            } else {
                SecKind::Movie
            };
            assert_eq!(BandFamily::of_sort(family.field(), true, kind, LibraryType::Primary), Some(family), "{family:?}");
        }
    }

    // ---- plan: the calendar ----

    fn edge(date: Day, basis: Basis) -> Edge {
        Edge::Day { date, basis }
    }

    #[test]
    fn dates_on_the_last_day_of_october_run_today_months_then_years() {
        let bands = plan(BandFamily::DateAdded, [2026, 10, 31], 2024);
        let mut expected = vec![(Mark::Today, edge([2026, 10, 31], Basis::Local))];
        for month in (1..=10u8).rev() {
            expected.push((Mark::Month(month), edge([2026, i32::from(month), 1], Basis::Local)));
        }
        expected.push((Mark::Year(2025), edge([2025, 1, 1], Basis::Local)));
        expected.push((Mark::Year(2024), edge([2024, 1, 1], Basis::Local)));
        let got: Vec<(Mark, Edge)> = bands.iter().map(|band| (band.mark(), band.edge())).collect();
        assert_eq!(got, expected);
        assert_eq!(bands.len(), 13);
    }

    #[test]
    fn on_new_years_day_there_is_today_then_years_and_no_empty_month() {
        let bands = plan(BandFamily::DateViewed, [2026, 1, 1], 2024);
        assert_eq!(marks(&bands), vec![Mark::Today, Mark::Year(2025), Mark::Year(2024)]);
        assert_eq!(bands[0].edge(), edge([2026, 1, 1], Basis::Local));
        assert_eq!(bands[1].edge(), edge([2025, 1, 1], Basis::Local));
    }

    #[test]
    fn a_month_with_no_day_before_today_is_skipped_but_the_month_before_is_not() {
        // 1 March: no day of March is before today; February is whole, and its length is the
        // caller's business (the edge is its first day).
        for (today, last_february) in [([2026, 3, 1], 28), ([2024, 3, 1], 29)] {
            let bands = plan(BandFamily::DateAdded, today, today[0]);
            assert_eq!(marks(&bands), vec![Mark::Today, Mark::Month(2), Mark::Month(1)]);
            assert_eq!(bands[1].edge(), edge([today[0], 2, 1], Basis::Local));
            assert_eq!(days([today[0], 3, 1]) - days([today[0], 2, 1]), last_february);
        }
    }

    #[test]
    fn the_second_of_the_month_keeps_the_month_of_one_day() {
        let bands = plan(BandFamily::DateAdded, [2026, 10, 2], 2026);
        assert_eq!(bands[1].mark(), Mark::Month(10));
        assert_eq!(bands.len(), 1 + 10);
    }

    #[test]
    fn a_leap_day_is_a_date_and_a_february_29_of_a_common_year_is_not() {
        let bands = plan(BandFamily::LastEpisodeAdded, [2028, 2, 29], 2026);
        assert_eq!(
            marks(&bands),
            vec![Mark::Today, Mark::Month(2), Mark::Month(1), Mark::Year(2027), Mark::Year(2026)]
        );
        assert_eq!(bands[0].edge(), edge([2028, 2, 29], Basis::Local));
        assert!(!plan(BandFamily::DateAdded, [2000, 2, 29], 1999).is_empty(), "2000 is a leap year");
        let impossible_days =
            [[2026, 2, 29], [2100, 2, 29], [2026, 4, 31], [2026, 13, 1], [2026, 0, 1], [2026, 1, 0], [2026, 1, 32]];
        for impossible in impossible_days {
            assert!(plan(BandFamily::DateAdded, impossible, 2020).is_empty(), "{impossible:?}");
            assert!(plan(BandFamily::Release, impossible, 2020).is_empty(), "{impossible:?}");
        }
        assert!(plan(BandFamily::DateAdded, [999, 6, 1], 900).is_empty());
    }

    #[test]
    fn month_lengths_decide_the_last_valid_day() {
        let lengths: Vec<i32> = (1..=12).map(|month| days_in_month(2026, month).unwrap()).collect();
        assert_eq!(lengths, vec![31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]);
        assert_eq!(days_in_month(2024, 2), Some(29));
        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2000, 2), Some(29));
        assert_eq!(days_in_month(2026, 0), None);
        assert_eq!(days_in_month(2026, 13), None);
        // Every month of a common year ends where the next one's first day begins.
        for month in 1..=11 {
            let end = days([2026, month, days_in_month(2026, month).unwrap()]);
            assert_eq!(days([2026, month + 1, 1]), end + 1, "month {month}");
        }
    }

    #[test]
    fn the_last_day_of_the_year_and_the_first_month() {
        let december = plan(BandFamily::DateAdded, [2026, 12, 31], 2026);
        assert_eq!(december.len(), 1 + 12);
        assert_eq!(december[1].mark(), Mark::Month(12));
        assert_eq!(december[12].mark(), Mark::Month(1));
        let january = plan(BandFamily::DateAdded, [2026, 1, 15], 2025);
        assert_eq!(marks(&january), vec![Mark::Today, Mark::Month(1), Mark::Year(2025)]);
    }

    #[test]
    fn no_year_band_when_the_oldest_item_is_this_year_or_newer() {
        for oldest in [2026, 2027, i32::MAX] {
            let bands = plan(BandFamily::DateAdded, [2026, 10, 31], oldest);
            assert_eq!(bands.len(), 11, "oldest {oldest}");
            assert!(bands.iter().all(|band| !matches!(band.mark(), Mark::Year(_))));
        }
    }

    #[test]
    fn a_bogus_oldest_year_stops_at_the_first_year_the_model_draws() {
        for oldest in [0, 1, -5, i32::MIN] {
            let bands = plan(BandFamily::DateAdded, [2026, 10, 31], oldest);
            assert_eq!(bands.last().map(Band::mark), Some(Mark::Year(EARLIEST_YEAR)), "oldest {oldest}");
            assert_eq!(bands.len(), 1 + 10 + (2026 - EARLIEST_YEAR) as usize);
        }
    }

    // ---- plan: release date ----

    #[test]
    fn release_dates_run_this_year_then_decades() {
        let bands = plan(BandFamily::Release, [2026, 10, 31], 1995);
        let got: Vec<(Mark, Edge)> = bands.iter().map(|band| (band.mark(), band.edge())).collect();
        assert_eq!(
            got,
            vec![
                (Mark::Year(2026), edge([2026, 1, 1], Basis::Utc)),
                (Mark::Decade(2020), edge([2020, 1, 1], Basis::Utc)),
                (Mark::Decade(2010), edge([2010, 1, 1], Basis::Utc)),
                (Mark::Decade(2000), edge([2000, 1, 1], Basis::Utc)),
                (Mark::Decade(1990), edge([1990, 1, 1], Basis::Utc)),
            ]
        );
    }

    #[test]
    fn the_decade_a_year_opens_has_no_band_of_its_own() {
        let bands = plan(BandFamily::Release, [2030, 5, 5], 2005);
        assert_eq!(
            marks(&bands),
            vec![Mark::Year(2030), Mark::Decade(2020), Mark::Decade(2010), Mark::Decade(2000)]
        );
        let bands = plan(BandFamily::Release, [2020, 1, 1], 2020);
        assert_eq!(marks(&bands), vec![Mark::Year(2020)]);
        let bands = plan(BandFamily::Release, [2029, 12, 31], 2021);
        assert_eq!(marks(&bands), vec![Mark::Year(2029), Mark::Decade(2020)]);
    }

    #[test]
    fn release_decades_stop_at_the_decade_of_the_oldest_year() {
        let bands = plan(BandFamily::Release, [2026, 6, 1], 1999);
        assert_eq!(marks(&bands).last(), Some(&Mark::Decade(1990)));
        let bands = plan(BandFamily::Release, [2026, 6, 1], 2026);
        assert_eq!(marks(&bands), vec![Mark::Year(2026)]);
        let bands = plan(BandFamily::Release, [2026, 6, 1], 2021);
        assert_eq!(marks(&bands), vec![Mark::Year(2026), Mark::Decade(2020)]);
        let bands = plan(BandFamily::Release, [2026, 6, 1], i32::MIN);
        assert_eq!(marks(&bands).last(), Some(&Mark::Decade(EARLIEST_YEAR)));
    }

    // ---- plan: the fixed ladders ----

    #[test]
    fn the_rating_ladder_ends_in_a_tail() {
        let bands = plan(BandFamily::Rating, [0; 3], 0);
        let got: Vec<(Mark, Edge)> = bands.iter().map(|band| (band.mark(), band.edge())).collect();
        assert_eq!(
            got,
            vec![
                (Mark::Percent(95), Edge::Rating(95)),
                (Mark::Percent(90), Edge::Rating(90)),
                (Mark::Percent(85), Edge::Rating(85)),
                (Mark::Percent(80), Edge::Rating(80)),
                (Mark::Percent(75), Edge::Rating(75)),
                (Mark::BelowPercent(75), Edge::Floor),
            ]
        );
    }

    #[test]
    fn the_duration_ladder_is_open_at_the_top_and_ends_in_a_tail() {
        let bands = plan(BandFamily::Duration, [0; 3], 0);
        let got: Vec<(Mark, Edge)> = bands.iter().map(|band| (band.mark(), band.edge())).collect();
        assert_eq!(
            got,
            vec![
                (Mark::MinutesOrMore(180), Edge::Minutes(180)),
                (Mark::Minutes(150), Edge::Minutes(150)),
                (Mark::Minutes(120), Edge::Minutes(120)),
                (Mark::Minutes(90), Edge::Minutes(90)),
                (Mark::Minutes(60), Edge::Minutes(60)),
                (Mark::BelowMinutes(60), Edge::Floor),
            ]
        );
    }

    #[test]
    fn the_count_ladders_have_no_tail_so_zero_has_no_band() {
        let unplayed = plan(BandFamily::Unplayed, [0; 3], 0);
        assert_eq!(
            marks(&unplayed),
            vec![
                Mark::CountOrMore(12),
                Mark::Count(10),
                Mark::Count(8),
                Mark::Count(6),
                Mark::Count(4),
                Mark::Count(2),
                Mark::Count(1)
            ]
        );
        assert_eq!(unplayed.last().map(Band::edge), Some(Edge::Count(1)));
        let plays = plan(BandFamily::Plays, [0; 3], 0);
        assert_eq!(
            marks(&plays),
            vec![Mark::CountOrMore(10), Mark::Count(5), Mark::Count(3), Mark::Count(2), Mark::Count(1)]
        );
        assert!(plays.iter().chain(&unplayed).all(|band| band.edge() != Edge::Floor && band.edge() != Edge::Count(0)));
    }

    #[test]
    fn a_fixed_ladder_ignores_today_and_the_oldest_year() {
        for family in [BandFamily::Rating, BandFamily::Duration, BandFamily::Unplayed, BandFamily::Plays] {
            assert_eq!(plan(family, [2026, 10, 31], 1980), plan(family, [0, 0, 0], i32::MIN), "{family:?}");
            assert_eq!(family.basis(), None);
        }
    }

    // ---- every plan ----

    const TODAYS: [Day; 4] = [[2026, 10, 31], [2026, 1, 1], [2028, 2, 29], [2030, 3, 1]];

    #[test]
    fn every_plan_is_strictly_ordered_by_its_edges() {
        for family in BandFamily::ALL {
            for today in TODAYS {
                for oldest in [1995, 2026, 2031] {
                    let bands = plan(family, today, oldest);
                    assert!(!bands.is_empty(), "{family:?} {today:?}");
                    for pair in bands.windows(2) {
                        assert!(
                            pair[0].edge() > pair[1].edge(),
                            "{family:?} {today:?} {oldest}: {:?} then {:?}",
                            pair[0],
                            pair[1]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_marks_of_a_plan_are_all_different() {
        for family in BandFamily::ALL {
            for today in TODAYS {
                let bands = plan(family, today, 1990);
                let distinct: HashSet<Mark> = bands.iter().map(Band::mark).collect();
                assert_eq!(distinct.len(), bands.len(), "{family:?} {today:?}");
            }
        }
    }

    #[test]
    fn the_basis_is_the_familys_and_every_date_edge_wears_it() {
        let expect = [
            (BandFamily::DateAdded, Some(Basis::Local)),
            (BandFamily::DateViewed, Some(Basis::Local)),
            (BandFamily::LastEpisodeAdded, Some(Basis::Local)),
            (BandFamily::Release, Some(Basis::Utc)),
            (BandFamily::Rating, None),
        ];
        for (family, basis) in expect {
            assert_eq!(family.basis(), basis);
            for band in plan(family, [2026, 10, 31], 2000) {
                if let Edge::Day { basis: wears, .. } = band.edge() {
                    assert_eq!(Some(wears), basis, "{family:?} {band:?}");
                }
            }
        }
    }

    // ---- the probe terms ----

    #[test]
    fn the_numeric_ladders_render_the_terms_measured_per_family() {
        // `>>=E-1unit`: rating edges minus 0.01, duration edges minus 1 ms, count edges minus 1.
        let rating = terms(&plan(BandFamily::Rating, [0; 3], 0));
        assert_eq!(rating[..5], ["rating>>=9.49", "rating>>=8.99", "rating>>=8.49", "rating>>=7.99", "rating>>=7.49"]);
        let duration = terms(&plan(BandFamily::Duration, [0; 3], 0));
        assert_eq!(
            duration[..5],
            [
                "duration>>=10799999",
                "duration>>=8999999",
                "duration>>=7199999",
                "duration>>=5399999",
                "duration>>=3599999"
            ]
        );
        assert_eq!(
            terms(&plan(BandFamily::Unplayed, [0; 3], 0)),
            [
                "unviewedLeafCount>>=11",
                "unviewedLeafCount>>=9",
                "unviewedLeafCount>>=7",
                "unviewedLeafCount>>=5",
                "unviewedLeafCount>>=3",
                "unviewedLeafCount>>=1",
                "unviewedLeafCount>>=0"
            ]
        );
        assert_eq!(
            terms(&plan(BandFamily::Plays, [0; 3], 0)),
            ["viewCount>>=9", "viewCount>>=4", "viewCount>>=2", "viewCount>>=1", "viewCount>>=0"]
        );
    }

    #[test]
    fn a_tail_band_is_probed_above_zero_which_leaves_out_stored_zeros_and_no_value_items() {
        // Measured: `>>=0` counted neither the unrated movies nor a stored zero, `>=0` counted the
        // stored zeros. The floor is the first of the two.
        assert_eq!(terms(&plan(BandFamily::Rating, [0; 3], 0))[5], "rating>>=0");
        assert_eq!(terms(&plan(BandFamily::Duration, [0; 3], 0))[5], "duration>>=0");
        // No number probe uses the inclusive operator, and the highest rung of a count ladder
        // (`>>=11`) is the same operator as its lowest (`>>=0`).
        for family in [BandFamily::Rating, BandFamily::Duration, BandFamily::Unplayed, BandFamily::Plays] {
            for probe in probes(&plan(family, [0; 3], 0), &fake_local).unwrap() {
                assert_eq!(probe.op, ">>", "{family:?} {}", probe.term());
            }
        }
    }

    #[test]
    fn an_instant_family_is_probed_at_local_midnight_exactly() {
        // The fake viewer is five hours behind UTC, so local midnight is UTC midnight + 18000, and
        // the edge is the epoch itself (`>=`), not the epoch minus a second.
        let added = terms(&plan(BandFamily::DateAdded, [2026, 10, 31], 2025));
        assert_eq!(added[0], "addedAt>=1793422800", "Today: 1793404800 + 18000");
        assert_eq!(added[1], "addedAt>=1790830800", "October: 1790812800 + 18000");
        assert_eq!(added.last().map(String::as_str), Some("addedAt>=1735707600"), "2025: 1735689600 + 18000");
        assert_eq!(terms(&plan(BandFamily::DateViewed, [2026, 10, 31], 2026))[0], "lastViewedAt>=1793422800");
        assert_eq!(terms(&plan(BandFamily::LastEpisodeAdded, [2026, 10, 31], 2026))[0], "episode.addedAt>=1793422800");
    }

    #[test]
    fn a_release_date_is_probed_at_utc_midnight_back_to_the_oldest_decade() {
        let bands = plan(BandFamily::Release, [2026, 10, 31], 1960);
        assert_eq!(
            marks(&bands),
            [Mark::Year(2026)].into_iter().chain((6..=12).rev().map(|n| Mark::Decade(1900 + 10 * n))).collect::<Vec<_>>()
        );
        let release = terms(&bands);
        // 2026-01-01 is 1767225600 in UTC; the local reading (+18000) would end in ...243600.
        assert_eq!(release[0], "originallyAvailableAt>=1767225600");
        assert_eq!(release[1], "originallyAvailableAt>=1577836800", "2020s");
        assert_eq!(release[4], "originallyAvailableAt>=631152000", "1990s");
        // The 1970s edge is plain zero, and the decades before 1970 are negative absolute epochs
        // (measured: a bare negative number on a date field is not a relative date).
        assert_eq!(release[6], "originallyAvailableAt>=0", "1970s");
        assert_eq!(release[7], "originallyAvailableAt>=-315619200", "1960s");
        assert_eq!(release.len(), 8);
    }

    #[test]
    fn a_utc_edge_never_asks_the_converter_and_a_local_edge_asks_it_once_without_a_basis() {
        use std::cell::RefCell;
        let asked = RefCell::new(Vec::new());
        let spy = |date: Day| {
            asked.borrow_mut().push(date);
            fake_local(date)
        };
        let release = plan(BandFamily::Release, [2026, 10, 31], 1001);
        assert!(probes(&release, &spy).is_some());
        assert!(asked.borrow().is_empty(), "release dates need no zone");
        let added = plan(BandFamily::DateAdded, [2026, 3, 1], 2025);
        assert!(probes(&added, &spy).is_some());
        assert_eq!(*asked.borrow(), vec![[2026, 3, 1], [2026, 2, 1], [2026, 1, 1], [2025, 1, 1]]);
        asked.borrow_mut().clear();
        assert!(probes(&plan(BandFamily::Rating, [0; 3], 0), &spy).is_some());
        assert!(asked.borrow().is_empty(), "a number edge needs no calendar");
    }

    #[test]
    fn a_failing_local_converter_leaves_no_probe_but_cannot_touch_a_release_date() {
        // A 32-bit `time_t` fails a local date before 1901: that is one band without a probe, and
        // `probes` is all or nothing. A release date before 1902 needs no converter at all.
        let bands = plan(BandFamily::DateAdded, [2026, 10, 31], 2024);
        let fails_on_2025 = |date: Day| if date[0] == 2025 { None } else { fake_local(date) };
        assert!(probes(&bands, &fails_on_2025).is_none());
        assert!(bands[0].probe(&fails_on_2025).is_some());
        let never = |_: Day| None;
        let ancient = plan(BandFamily::Release, [2026, 10, 31], 1850);
        let terms = probes(&ancient, &never).expect("UTC edges are computed here").iter().map(Probe::term).collect::<Vec<_>>();
        assert_eq!(terms.last().map(String::as_str), Some("originallyAvailableAt>=-3786825600"), "1850s: 1850-01-01");
        assert_eq!(utc_midnight([1850, 1, 1]), -3_786_825_600);
    }

    #[test]
    fn a_probe_names_its_parts_and_offers_them_as_a_query_pair() {
        let bands = plan(BandFamily::LastEpisodeAdded, [2026, 10, 31], 2026);
        let probe = bands[0].probe(&fake_local).unwrap();
        assert_eq!((probe.field, probe.op, probe.value.as_str()), ("episode.addedAt", ">", "1793422800"));
        assert_eq!(probe.query_pair(), ("episode.addedAt>".to_owned(), "1793422800"));
        // The request builder writes `{key}={value}`: key and value rejoin as the term, with ONE `=`.
        let rating = plan(BandFamily::Rating, [0; 3], 0);
        let pairs: Vec<(String, String)> = probes(&rating, &fake_local)
            .unwrap()
            .iter()
            .map(|p| {
                let (key, value) = p.query_pair();
                (key, value.to_owned())
            })
            .collect();
        assert_eq!(pairs[0], ("rating>>".to_owned(), "9.49".to_owned()));
        assert_eq!(pairs[5], ("rating>>".to_owned(), "0".to_owned()), "the tail band's floor");
        for (key, value) in &pairs {
            assert!(!key.ends_with('='), "the query string owns the `=`: {key}");
            assert_eq!(format!("{key}={value}").matches('=').count(), 1);
        }
        let release = plan(BandFamily::Release, [2026, 10, 31], 2026);
        assert_eq!(release[0].probe(&never_asked).unwrap().query_pair(), ("originallyAvailableAt>".to_owned(), "1767225600"));
        assert_eq!(hundredths(949), "9.49");
        assert_eq!(hundredths(700), "7.00");
        assert_eq!(hundredths(5), "0.05");
        assert_eq!(hundredths(-1), "-0.01");
        assert_eq!(hundredths(-250), "-2.50");
    }

    fn never_asked(date: Day) -> Option<i64> {
        panic!("a UTC edge asked the converter for {date:?}")
    }

    // ---- offsets ----

    fn rating() -> Vec<Band> {
        plan(BandFamily::Rating, [0; 3], 0)
    }

    /// Counts of items at or above each rating edge, with 100 items of which 10 have no rating.
    const RATING_COUNTS: [u32; 6] = [5, 12, 30, 41, 55, 90];

    #[test]
    fn a_descending_listing_starts_each_band_where_the_higher_ones_end() {
        let layout = offsets(&rating(), &RATING_COUNTS, 100, true).unwrap();
        let got: Vec<(u32, u32)> = layout.bands.iter().map(|b| (b.start, b.size)).collect();
        assert_eq!(got, [(0, 5), (5, 7), (12, 18), (30, 11), (41, 14), (55, 35)]);
        assert_eq!((layout.lead, layout.tail), (0, 10));
        assert_eq!(marks(&rating()), layout.bands.iter().map(|b| b.band.mark()).collect::<Vec<_>>());
        assert_eq!(layout.populated(), 6);
    }

    #[test]
    fn an_ascending_listing_reads_the_bands_lowest_first_after_the_unrated() {
        let layout = offsets(&rating(), &RATING_COUNTS, 100, false).unwrap();
        let got: Vec<(u32, u32)> = layout.bands.iter().map(|b| (b.start, b.size)).collect();
        assert_eq!(got, [(10, 35), (45, 14), (59, 11), (70, 18), (88, 7), (95, 5)]);
        assert_eq!((layout.lead, layout.tail), (10, 0));
        assert_eq!(
            layout.bands.iter().map(|b| b.band.mark()).collect::<Vec<_>>(),
            vec![
                Mark::BelowPercent(75),
                Mark::Percent(75),
                Mark::Percent(80),
                Mark::Percent(85),
                Mark::Percent(90),
                Mark::Percent(95)
            ]
        );
    }

    #[test]
    fn the_two_directions_are_mirror_images_and_cover_the_listing() {
        let cases: [(BandFamily, &[u32], u32); 4] = [
            (BandFamily::Rating, &RATING_COUNTS, 100),
            (BandFamily::Duration, &[0, 0, 3, 3, 17, 60], 75),
            (BandFamily::Unplayed, &[1, 2, 2, 9, 9, 20, 31], 44),
            (BandFamily::Plays, &[4, 4, 4, 10, 12], 12),
        ];
        for (family, counts, total) in cases {
            let bands = plan(family, [0; 3], 0);
            let desc = offsets(&bands, counts, total, true).unwrap();
            let asc = offsets(&bands, counts, total, false).unwrap();
            assert_eq!(desc.tail, asc.lead, "{family:?}: the same unlabelled items");
            assert_eq!((desc.lead, asc.tail), (0, 0));
            for (i, high_first) in desc.bands.iter().enumerate() {
                let mirrored = &asc.bands[desc.bands.len() - 1 - i];
                assert_eq!(high_first.band, mirrored.band, "{family:?} {i}");
                assert_eq!(high_first.size, mirrored.size, "{family:?} {i}");
                assert_eq!(high_first.start + high_first.size + mirrored.start, total, "{family:?} {i}");
            }
            for layout in [&desc, &asc] {
                let sized: u32 = layout.bands.iter().map(|b| b.size).sum();
                assert_eq!(layout.lead + sized + layout.tail, total, "{family:?}");
                // Each band begins where the one before it ends, from the lead.
                let mut next = layout.lead;
                for band in &layout.bands {
                    assert_eq!(band.start, next, "{family:?} {:?}", band.band.mark());
                    next += band.size;
                }
                assert_eq!(next + layout.tail, total);
            }
        }
    }

    #[test]
    fn an_empty_band_starts_where_the_next_one_does() {
        let bands = plan(BandFamily::Duration, [0; 3], 0);
        let layout = offsets(&bands, &[0, 0, 3, 3, 17, 60], 75, true).unwrap();
        let got: Vec<(u32, u32)> = layout.bands.iter().map(|b| (b.start, b.size)).collect();
        assert_eq!(got, [(0, 0), (0, 0), (0, 3), (3, 0), (3, 14), (17, 43)]);
        assert_eq!(layout.populated(), 3);
        assert_eq!(layout.tail, 15);
    }

    #[test]
    fn a_ladder_without_a_tail_leaves_its_zeros_unlabelled() {
        // 44 shows, 31 with something left to watch: 13 with none belong to no band.
        let bands = plan(BandFamily::Unplayed, [0; 3], 0);
        let counts = [1, 2, 2, 9, 9, 20, 31];
        let desc = offsets(&bands, &counts, 44, true).unwrap();
        assert_eq!(desc.tail, 13);
        let asc = offsets(&bands, &counts, 44, false).unwrap();
        assert_eq!(asc.lead, 13);
        assert_eq!(asc.bands[0].band.mark(), Mark::Count(1));
        assert_eq!(asc.bands[0].start, 13);
    }

    #[test]
    fn counts_without_any_unlabelled_item_have_neither_lead_nor_tail() {
        let desc = offsets(&rating(), &[5, 12, 30, 41, 55, 100], 100, true).unwrap();
        assert_eq!((desc.lead, desc.tail), (0, 0));
        let asc = offsets(&rating(), &[5, 12, 30, 41, 55, 100], 100, false).unwrap();
        assert_eq!((asc.lead, asc.tail), (0, 0));
        assert_eq!(asc.bands[0].start, 0);
    }

    /// The model against a listing it has to agree with: build the items, order them the way the
    /// server does (no value last in `:desc`, first in `:asc`), count them the way a probe does, and
    /// find where each band really begins by reading the order.
    fn listing_agrees(family: BandFamily, today: Day, oldest: i32, value_of: &dyn Fn(Edge) -> i64, items: &[Option<i64>]) {
        let bands = plan(family, today, oldest);
        let counts: Vec<u32> = bands
            .iter()
            .map(|band| items.iter().flatten().filter(|&&item| item >= value_of(band.edge())).count() as u32)
            .collect();
        let total = items.len() as u32;
        // The band of an item: the first (highest) whose edge it reaches.
        let band_of = |item: &Option<i64>| -> Option<usize> {
            let item = (*item)?;
            bands.iter().position(|band| item >= value_of(band.edge()))
        };
        for descending in [true, false] {
            let mut listing: Vec<Option<i64>> = items.to_vec();
            // `None` is the smallest value: last when descending, first when ascending.
            listing.sort_by(|a, b| if descending { b.cmp(a) } else { a.cmp(b) });
            let layout = offsets(&bands, &counts, total, descending).expect("a consistent probe");
            let order: Vec<usize> = if descending { (0..bands.len()).collect() } else { (0..bands.len()).rev().collect() };
            for (shown, &band_index) in order.iter().enumerate() {
                let start = &layout.bands[shown];
                assert_eq!(start.band, bands[band_index]);
                let members: Vec<usize> =
                    (0..listing.len()).filter(|&at| band_of(&listing[at]) == Some(band_index)).collect();
                assert_eq!(start.size as usize, members.len(), "{family:?} desc={descending} {:?}", start.band.mark());
                if let (Some(&first), Some(&last)) = (members.first(), members.last()) {
                    assert_eq!(start.start as usize, first, "{family:?} desc={descending} {:?}", start.band.mark());
                    assert_eq!(last + 1 - first, members.len(), "a band is one run");
                }
            }
            let unbanded: Vec<usize> = (0..listing.len()).filter(|&at| band_of(&listing[at]).is_none()).collect();
            let (edge_count, expected_at) = if descending { (layout.tail, total - layout.tail) } else { (layout.lead, 0) };
            assert_eq!(edge_count as usize, unbanded.len(), "{family:?} desc={descending}");
            if let Some(&first) = unbanded.first() {
                assert_eq!(first as u32, expected_at, "{family:?} desc={descending}: the unlabelled run");
            }
        }
    }

    #[test]
    fn rating_offsets_find_the_rows_of_a_listing_with_values_on_the_edges() {
        // Tenths of a point: 95 is 9.5. Edge values, one below them, a zero rating and unrated items.
        let items: Vec<Option<i64>> = [99, 95, 95, 94, 90, 89, 85, 85, 84, 80, 79, 75, 74, 50, 0]
            .iter()
            .map(|&tenths| Some(tenths))
            .chain([None, None, None])
            .collect();
        let value_of = |edge: Edge| match edge {
            Edge::Rating(tenths) => i64::from(tenths),
            // `>>=0`: a value above zero. The unit is a tenth, so that is 1; a stored 0 is left out.
            Edge::Floor => 1,
            other => panic!("{other:?}"),
        };
        listing_agrees(BandFamily::Rating, [0; 3], 0, &value_of, &items);
    }

    #[test]
    fn duration_offsets_find_the_rows_of_a_listing_with_values_a_millisecond_apart() {
        let minute = 60_000_i64;
        let items: Vec<Option<i64>> = [
            200 * minute,
            180 * minute,
            180 * minute - 1,
            150 * minute,
            150 * minute - 1,
            91 * minute,
            90 * minute,
            61 * minute,
            60 * minute,
            60 * minute - 1,
            minute,
            0,
        ]
        .iter()
        .map(|&ms| Some(ms))
        .chain([None])
        .collect();
        let value_of = |edge: Edge| match edge {
            Edge::Minutes(minutes) => i64::from(minutes) * minute,
            // `>>=0`: a value above zero. The unit is a millisecond, so that is 1; a stored 0 is left out.
            Edge::Floor => 1,
            other => panic!("{other:?}"),
        };
        listing_agrees(BandFamily::Duration, [0; 3], 0, &value_of, &items);
    }

    #[test]
    fn count_offsets_leave_the_zeros_to_no_band() {
        let unplayed: Vec<Option<i64>> =
            [0, 0, 0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 40].iter().map(|&n| Some(n)).collect();
        let value_of = |edge: Edge| match edge {
            Edge::Count(n) => i64::from(n),
            other => panic!("{other:?}"),
        };
        listing_agrees(BandFamily::Unplayed, [0; 3], 0, &value_of, &unplayed);
        let plays: Vec<Option<i64>> = [0, 0, 1, 2, 2, 3, 4, 5, 9, 10, 11].iter().map(|&n| Some(n)).collect();
        listing_agrees(BandFamily::Plays, [0; 3], 0, &value_of, &plays);
    }

    #[test]
    fn date_offsets_find_the_rows_of_a_listing_with_items_on_either_side_of_midnight() {
        let today = [2026, 10, 31];
        let at = |date: Day, extra: i64| days(date) * 86_400 + LOCAL_ZONE_SECONDS + extra;
        // Added today at local midnight and one second before it, on the first of a month, in a
        // past year; and never-viewed (no value) items.
        let items: Vec<Option<i64>> = vec![
            Some(at(today, 3600)),
            Some(at(today, 0)),
            Some(at(today, -1)),
            Some(at([2026, 10, 1], 0)),
            Some(at([2026, 9, 30], 86_399)),
            Some(at([2026, 1, 1], 0)),
            Some(at([2025, 12, 31], 86_399)),
            Some(at([2025, 1, 1], 0)),
            Some(at([2024, 6, 15], 0)),
            None,
            None,
        ];
        listing_agrees(BandFamily::DateViewed, today, 2024, &epoch_of, &items);
    }

    #[test]
    fn release_offsets_find_the_rows_of_a_listing_in_utc_days() {
        let utc_day = |date: Day| Some(days(date) * 86_400);
        let items: Vec<Option<i64>> = vec![
            utc_day([2026, 3, 1]),
            utc_day([2026, 1, 1]),
            utc_day([2025, 12, 31]),
            utc_day([2020, 1, 1]),
            utc_day([2019, 12, 31]),
            utc_day([2001, 5, 5]),
            utc_day([1999, 12, 31]),
            utc_day([1995, 7, 7]),
            utc_day([1969, 12, 31]),
            None,
        ];
        // 1969-12-31 is before 1970: its epoch is negative and still falls in the 1960s band.
        listing_agrees(BandFamily::Release, [2026, 10, 31], 1969, &epoch_of, &items);
    }

    #[test]
    fn releases_before_1970_have_their_own_bands_and_a_clamping_server_would_only_leave_them_unlabelled() {
        let utc_day = |date: Day| Some(days(date) * 86_400);
        let items: Vec<Option<i64>> = vec![
            utc_day([2026, 3, 1]),
            utc_day([2001, 5, 5]),
            utc_day([1975, 7, 7]),
            utc_day([1969, 12, 31]),
            utc_day([1960, 1, 1]),
            utc_day([1955, 5, 5]),
            None,
        ];
        let today = [2026, 10, 31];
        let counts_under = |value_of: &dyn Fn(Edge) -> i64| -> Offsets {
            let bands = plan(BandFamily::Release, today, 1949);
            let counts: Vec<u32> = bands
                .iter()
                .map(|band| items.iter().flatten().filter(|&&v| v >= value_of(band.edge())).count() as u32)
                .collect();
            offsets(&bands, &counts, items.len() as u32, true).expect("a consistent probe")
        };
        let size_of = |layout: &Offsets, decade: i32| {
            layout.bands.iter().find(|b| b.band.mark() == Mark::Decade(decade)).map(|b| b.size)
        };
        // Absolute negative epochs (what was measured): the 1960s and 1950s hold their releases.
        listing_agrees(BandFamily::Release, today, 1949, &epoch_of, &items);
        let real = counts_under(&epoch_of);
        assert_eq!((size_of(&real, 1960), size_of(&real, 1950), size_of(&real, 1940)), (Some(2), Some(1), Some(0)));
        assert_eq!(real.tail, 1, "only the release without a date");
        // A server that clamped a negative edge to zero: the old bands are empty, the old items fall
        // into the unlabelled tail with the dateless one, and nothing is mislabelled.
        let clamped = |edge: Edge| epoch_of(edge).max(0);
        listing_agrees(BandFamily::Release, today, 1949, &clamped, &items);
        let flat = counts_under(&clamped);
        assert_eq!((size_of(&flat, 1960), size_of(&flat, 1950), size_of(&flat, 1940)), (Some(0), Some(0), Some(0)));
        assert_eq!(flat.tail, 4, "three releases before 1970 and one without a date");
        assert_eq!(size_of(&flat, 1970), size_of(&real, 1970), "the bands from 1970 on are unchanged");
    }

    // ---- rejection ----

    #[test]
    fn a_count_sequence_that_is_not_monotone_is_rejected() {
        assert_eq!(
            offsets(&rating(), &[5, 12, 11, 41, 55, 90], 100, true),
            Err(Reject::NotMonotone { index: 2 })
        );
        // A rating ladder read in the wrong order (lowest edge first) decreases at once.
        assert_eq!(offsets(&rating(), &[90, 55, 41, 30, 12, 5], 100, false), Err(Reject::NotMonotone { index: 1 }));
        // Equal neighbours are fine: an empty band.
        assert!(offsets(&rating(), &[5, 5, 30, 41, 55, 90], 100, true).is_ok());
    }

    #[test]
    fn a_count_above_the_total_is_rejected_before_anything_else() {
        assert_eq!(offsets(&rating(), &[5, 12, 30, 41, 55, 101], 100, true), Err(Reject::AboveTotal { index: 5 }));
        // Non-monotone as well, but the listing it counts is the wrong one: that is said first.
        assert_eq!(offsets(&rating(), &[200, 12, 30, 41, 55, 90], 100, true), Err(Reject::AboveTotal { index: 0 }));
        assert_eq!(offsets(&rating(), &[0, 0, 0, 0, 0, 1], 0, false), Err(Reject::AboveTotal { index: 5 }));
    }

    #[test]
    fn counts_that_all_equal_the_total_are_an_ignored_filter() {
        assert_eq!(offsets(&rating(), &[44; 6], 44, true), Err(Reject::AllEqualTotal));
        assert_eq!(offsets(&rating(), &[44; 6], 44, false), Err(Reject::AllEqualTotal));
        // An empty listing answers zero everywhere: nothing to show either.
        assert_eq!(offsets(&rating(), &[0; 6], 0, true), Err(Reject::AllEqualTotal));
        // One count short of the total is a real probe: the lowest band holds every rated item.
        let ok = offsets(&rating(), &[44, 44, 44, 44, 44, 44], 45, true).unwrap();
        assert_eq!((ok.tail, ok.populated()), (1, 1));
        assert_eq!(ok.bands[0].size, 44);
    }

    #[test]
    fn a_wrong_number_of_counts_is_rejected() {
        assert_eq!(
            offsets(&rating(), &[5, 12, 30, 41, 55], 100, true),
            Err(Reject::WrongLength { bands: 6, counts: 5 })
        );
        assert_eq!(
            offsets(&rating(), &[5, 12, 30, 41, 55, 90, 90], 100, false),
            Err(Reject::WrongLength { bands: 6, counts: 7 })
        );
        assert_eq!(offsets(&rating(), &[], 100, true), Err(Reject::WrongLength { bands: 6, counts: 0 }));
    }

    #[test]
    fn no_bands_and_no_counts_is_rejected_not_accepted_as_everything_equal() {
        assert_eq!(offsets(&[], &[], 10, true), Err(Reject::NoBands));
        assert_eq!(offsets(&[], &[], 0, false), Err(Reject::NoBands));
    }

    #[test]
    fn a_rail_whose_bands_are_all_empty_is_a_valid_layout_that_shows_nothing() {
        // Nobody rated anything: monotone, within the total, not all equal to it. The layout is
        // honest (every item is unlabelled); the caller reads `populated()` to hide the rail.
        let layout = offsets(&rating(), &[0; 6], 12, false).unwrap();
        assert_eq!((layout.lead, layout.tail, layout.populated()), (12, 0, 0));
    }

    /// The rail draws every family, the calendar ones included; only the families whose edges are
    /// plain numbers have a ladder that needs no clock, and that ladder is the plan the model makes.
    #[test]
    fn the_rail_is_ready_for_every_family_and_only_the_fixed_ones_have_a_ladder_without_a_day() {
        assert_eq!(BandFamily::ALL.into_iter().filter(|f| f.rail_ready()).count(), BandFamily::ALL.len());
        for family in BandFamily::ALL {
            assert!(family.rail_ready(), "{family:?}");
            assert_eq!(family.fixed_ladder().is_some(), family.basis().is_none(), "{family:?}: a ladder without a day is `no calendar edge`");
        }
        for family in [BandFamily::DateAdded, BandFamily::DateViewed, BandFamily::LastEpisodeAdded, BandFamily::Release] {
            assert_eq!(family.fixed_ladder(), None, "{family:?} needs today and the oldest year, never year 1000");
        }
        assert_eq!(BandFamily::Rating.fixed_ladder(), Some(rating()));
        assert_eq!(BandFamily::Plays.fixed_ladder().unwrap().len(), 5);
    }

    #[test]
    fn a_watch_moves_the_play_and_unplayed_counts_and_not_the_rating() {
        let moving: Vec<BandFamily> = BandFamily::ALL.into_iter().filter(|f| f.moves_with_viewing()).collect();
        assert_eq!(moving, [BandFamily::DateViewed, BandFamily::Unplayed, BandFamily::Plays]);
    }

    /// Both directions are laid out at once, each as `offsets` lays it out alone, and a Reject is
    /// the same whichever the page asks.
    #[test]
    fn a_band_rail_holds_both_directions_and_rejects_alike() {
        let counts = [1, 3, 4, 6, 8, 9];
        let rail = BandRail::new(&rating(), &counts, 12).unwrap();
        assert_eq!(rail.listed(true), &offsets(&rating(), &counts, 12, true).unwrap());
        assert_eq!(rail.listed(false), &offsets(&rating(), &counts, 12, false).unwrap());
        assert_ne!(rail.listed(true), rail.listed(false));
        assert_eq!(BandRail::new(&rating(), &[12; 6], 12), Err(Reject::AllEqualTotal));
        assert_eq!(BandRail::new(&rating(), &[1, 3, 2, 6, 8, 9], 12), Err(Reject::NotMonotone { index: 2 }));
    }
}
