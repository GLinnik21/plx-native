//! The Seerr title page's two decisions: what the pill does and what the status line says.
use super::*;
use plx_data::seerr::{Season, Title};

fn movie(status: Availability) -> TitleDetail {
    TitleDetail { title: Title { kind: Some(MediaKind::Movie), tmdb: 1, status, ..Default::default() }, seasons: vec![] }
}

fn show(statuses: &[Availability]) -> TitleDetail {
    TitleDetail {
        title: Title { kind: Some(MediaKind::Tv), tmdb: 2, status: Availability::PartlyAvailable, ..Default::default() },
        seasons: statuses.iter().enumerate()
            .map(|(i, &status)| Season { number: i as i32 + 1, episodes: Some(8), status }).collect(),
    }
}

#[test]
fn a_movie_nobody_asked_for_can_be_requested_and_nothing_else_can() {
    assert_eq!(pill_state(&movie(Availability::Unknown), None), PillState::Request(vec![]));
    for status in [Availability::Pending, Availability::Processing, Availability::Available, Availability::Blocked] {
        assert_eq!(pill_state(&movie(status), None), PillState::None, "{status:?}");
    }
}

#[test]
fn a_show_requests_only_the_seasons_nobody_holds_or_asked_for() {
    let detail = show(&[Availability::Available, Availability::Unknown, Availability::Pending, Availability::Unknown]);
    assert_eq!(pill_state(&detail, None), PillState::Request(vec![2, 4]));
    assert_eq!(pill_state(&show(&[Availability::Available, Availability::Processing]), None), PillState::None);
}

#[test]
fn a_request_in_flight_or_done_takes_the_pill_and_a_failed_one_offers_it_again() {
    let detail = movie(Availability::Unknown);
    assert_eq!(pill_state(&detail, Some(&Answer::Loading)), PillState::Sending);
    assert_eq!(pill_state(&detail, Some(&Answer::Ready(SeerrData::Requested))), PillState::None);
    assert_eq!(pill_state(&detail, Some(&Answer::Failed(SeerrError::Transport))), PillState::Request(vec![]));
}

#[test]
fn the_status_line_says_the_request_first_then_the_server() {
    let detail = movie(Availability::Unknown);
    assert_eq!(status_line(&detail, None), None, "a title nobody asked for has no status");
    assert_eq!(status_line(&detail, Some(&Answer::Ready(SeerrData::Requested))), Some(msg::seerr_request_done()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::AlreadyRequested))), Some(msg::seerr_request_already()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::Forbidden))), Some(msg::seerr_request_forbidden()));
    assert_eq!(status_line(&detail, Some(&Answer::Failed(SeerrError::Transport))), Some(msg::seerr_request_failed()));
    assert_eq!(status_line(&movie(Availability::Processing), None), Some(msg::seerr_status_processing()));
    assert_eq!(status_line(&movie(Availability::Available), Some(&Answer::Loading)), Some(msg::seerr_status_available()));
}
