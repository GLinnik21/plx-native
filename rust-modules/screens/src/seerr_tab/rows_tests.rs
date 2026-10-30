//! A request row's words: the chip for every state, the seasons, who asked and answered, the
//! download line, and what a row says before its title has landed.
use super::*;
use plx_data::seerr::{Download, Title};

fn request(status: RequestStatus, availability: Availability) -> MediaRequest {
    MediaRequest {
        id: 7, kind: MediaKind::Movie, tmdb: 70, status, availability, seasons: Vec::new(),
        requested_by: Some("s0000a1ex".into()), modified_by: None, created: Some("2026-10-28T09:15:00.000Z".into()),
        updated: None, downloads: Vec::new(),
    }
}

fn downloading(size: i64, left: i64, state: DownloadState) -> Download {
    Download { title: String::new(), size, size_left: left, time_left_ms: Some(12 * 60_000), state, episode: None }
}

#[test]
fn the_chip_says_what_became_of_the_request() {
    use Availability as A;
    use RequestStatus as R;
    let cases = [
        (R::PendingApproval, A::Pending, msg::seerr_chip_pending()),
        (R::Declined, A::Unknown, msg::seerr_chip_declined()),
        (R::Failed, A::Processing, msg::seerr_chip_failed()),
        (R::Approved, A::Pending, msg::seerr_chip_approved()),
        (R::Approved, A::Processing, msg::seerr_chip_processing()),
        (R::Approved, A::PartlyAvailable, msg::seerr_chip_partly()),
        (R::Approved, A::Available, msg::seerr_chip_available()),
        (R::Completed, A::Unknown, msg::seerr_chip_available()),
    ];
    for (status, availability, want) in cases {
        assert_eq!(chip(&request(status, availability)), want, "{status:?} {availability:?}");
    }
    let moving = MediaRequest { downloads: vec![downloading(10, 5, DownloadState::Downloading)],
        ..request(RequestStatus::Approved, Availability::Processing) };
    assert_eq!(chip(&moving), msg::seerr_chip_downloading());
    let queued = MediaRequest { downloads: vec![downloading(10, 10, DownloadState::Queued)], ..moving.clone() };
    assert_eq!(chip(&queued), msg::seerr_chip_processing(), "a queued download is not moving yet");
}

#[test]
fn a_row_names_its_kind_seasons_who_asked_and_when() {
    let _g = plx_base::testlock::serial();
    let show = MediaRequest { kind: MediaKind::Tv, seasons: vec![1, 2], ..request(RequestStatus::PendingApproval, Availability::Pending) };
    let detail = TitleDetail {
        title: Title { kind: Some(MediaKind::Tv), tmdb: 70, title: "s0000ab1e".into(), year: Some(2024),
            poster: Some("/p.jpg".into()), ..Default::default() },
        ..Default::default()
    };
    let view = row_view(&show, Some(&detail));
    assert_eq!(view.title, "s0000ab1e");
    assert_eq!(view.lines[0], format!("{} · 2024 · {}", msg::browse_kind_tv_show(), msg::seerr_row_seasons("1, 2")));
    assert_eq!(view.lines[1], format!("{} · {}", msg::seerr_row_requested_by("s0000a1ex"), fmt::pretty_date("2026-10-28", 0)));
    assert_eq!(view.lines[2], "", "nobody answered it yet");
    assert!(view.pending && !view.moving);
    assert_eq!(view.poster.as_deref(), Some("https://image.tmdb.org/t/p/w342/p.jpg"));
    let one = MediaRequest { seasons: vec![3], ..show };
    assert_eq!(row_view(&one, None).lines[0], format!("{} · {}", msg::browse_kind_tv_show(), fmt::season_identity(3, "")));
}

#[test]
fn before_its_title_lands_a_row_still_says_what_the_request_knows() {
    let _g = plx_base::testlock::serial();
    let view = row_view(&request(RequestStatus::Approved, Availability::Processing), None);
    assert_eq!((view.title.as_str(), view.poster.as_ref()), ("", None));
    assert_eq!(view.lines[0], msg::browse_kind_movie());
    assert!(!view.lines[1].is_empty());
    assert!(view.moving, "approved and not yet on the server: the page keeps reading the list");
}

#[test]
fn who_answered_is_said_until_a_download_takes_the_line() {
    let _g = plx_base::testlock::serial();
    let approved = MediaRequest { modified_by: Some("s00000wnr".into()), ..request(RequestStatus::Approved, Availability::Processing) };
    assert_eq!(row_view(&approved, None).lines[2], msg::seerr_row_approved_by("s00000wnr"));
    let declined = MediaRequest { status: RequestStatus::Declined, ..approved.clone() };
    assert_eq!(row_view(&declined, None).lines[2], msg::seerr_row_declined_by("s00000wnr"));
    let gib = 1024 * 1024 * 1024;
    let moving = MediaRequest { downloads: vec![downloading(4 * gib, 3 * gib, DownloadState::Downloading)], ..approved };
    let view = row_view(&moving, None);
    let of = msg::seerr_row_of(&fmt::bytes(gib, (1, 0, 0)), &fmt::bytes(4 * gib, (1, 0, 0)));
    assert_eq!(view.lines[2], format!("{} · {of} · {}", fmt::percent(25), fmt::time_left(12 * 60_000)));
    assert_eq!(view.progress, Some(0.25));
    let paused = MediaRequest { downloads: vec![downloading(4 * gib, 3 * gib, DownloadState::Paused)], ..moving };
    assert_eq!(row_view(&paused, None).lines[2], format!("{} · {of}", fmt::percent(25)),
        "time left is said only while it moves");
}
