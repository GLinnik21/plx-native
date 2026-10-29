//! The Seerr title page's status line. What its pill offers is the store's decision
//! (`plx_data::stores::seerr::offer`), tested there.
use super::*;
use plx_data::seerr::Title;

fn movie(status: Availability) -> TitleDetail {
    TitleDetail { title: Title { kind: Some(MediaKind::Movie), tmdb: 1, status, ..Default::default() }, seasons: vec![] }
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
