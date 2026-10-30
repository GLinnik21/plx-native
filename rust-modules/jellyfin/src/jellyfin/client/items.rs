//! One item and what hangs off it: its page, children, queue, watched state, subtitles and playback reports.

use super::*;

/// The key in the display preferences' `CustomPrefs` holding the dismissed Next Up episodes.
const DISMISSED_KEY: &str = "dismissedNextUp";
/// How many dismissed episodes are kept.
pub(super) const DISMISSED_MAX: usize = 50;

/// The ids in a display-preferences answer's dismissed list (comma-separated).
fn dismissed_in(prefs: &serde_json::Value) -> Vec<String> {
    prefs["CustomPrefs"][DISMISSED_KEY]
        .as_str()
        .map(|s| s.split(',').filter(|id| !id.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

impl JfClient {
    // ---- items ----

    /// One item with everything the detail page shows (`Client::metadata`), with its skip markers.
    /// The item and its segments are asked for together: a page waits for both, and a show or a
    /// season, which has none, answers an empty list.
    pub fn metadata(&self, item_id: &str) -> Option<Metadata> {
        let (dto, segs) = both(
            || self.get::<BaseItemDto>(&paths::item(self.user_id(), item_id)),
            || self.get::<QueryResult<MediaSegmentDto>>(&paths::media_segments(item_id)),
        );
        let dto = dto?;
        let segs = segs.flatten().map(|page| page.items);
        *self.recent_item.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((item_id.to_owned(), Instant::now(), dto.clone(), segs.clone()));
        self.item_page(&dto, segs.as_deref())
    }

    /// The item as [`Self::metadata`] read it in the last [`RECENT_ITEM`], else read now.
    pub fn metadata_recent(&self, item_id: &str) -> Option<Metadata> {
        match self.recent(item_id) {
            Some((dto, segs)) => self.item_page(&dto, segs.as_deref()),
            None => self.metadata(item_id),
        }
    }

    /// The item and its segments as [`Self::metadata`] read them in the last [`RECENT_ITEM`].
    fn recent(&self, item_id: &str) -> Option<(BaseItemDto, Option<Vec<MediaSegmentDto>>)> {
        self.with_recent(item_id, |dto, segs| (dto.clone(), segs.map(<[_]>::to_vec)))
    }

    /// `f` of the item [`Self::recent`] would give, without copying it. Runs under the item's
    /// lock, so `f` sends nothing.
    fn with_recent<R>(&self, item_id: &str, f: impl FnOnce(&BaseItemDto, Option<&[MediaSegmentDto]>) -> R) -> Option<R> {
        self.recent_item.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
            .filter(|(id, at, ..)| id == item_id && at.elapsed() < RECENT_ITEM)
            .map(|(_, _, dto, segs)| f(dto, segs.as_deref()))
    }

    /// The item's page from what the server said of it and its segments.
    fn item_page(&self, dto: &BaseItemDto, segs: Option<&[MediaSegmentDto]>) -> Option<Metadata> {
        let mut m = translate::metadata(dto, &self.ctx(None))?;
        if matches!(m.kind.as_str(), "movie" | "episode") {
            // Segments are optional: a server without a segment provider answers an empty list,
            // and an older server a 404, and both mean "no markers".
            if let Some(segs) = segs {
                m.marker = translate::markers(segs, m.duration);
            }
            // No segment provider: the file's own chapter names, when they say intro or credits.
            if m.marker.is_empty() {
                m.marker = translate::chapter_markers(&m.chapter, m.duration);
            }
        }
        if m.kind == "show" {
            m.on_deck = self.on_deck(&m.rating_key).map(|e| OnDeckHub { metadata: Some(Box::new(e)) });
        }
        Some(m)
    }

    /// The scrubbing thumbnails of the item's version that plays, part `part_id`
    /// (`Client::trickplay`; the server's first version when no source is that part). Read from the
    /// item the playback started from when it is still held ([`Self::recent`]).
    pub fn trickplay(&self, item_id: &str, part_id: i64) -> Option<Trickplay> {
        self.with_trickplay_item(item_id, |dto| translate::trickplay(dto, translate::source_of_part(dto, part_id)))
    }

    /// One sheet of the thumbnails `trickplay` described for the same part, as the server's JPEG
    /// (`Client::trickplay_sheet`).
    pub fn trickplay_sheet(&self, item_id: &str, part_id: i64, info: &Trickplay, sheet: u32) -> Option<Vec<u8>> {
        let source = self.with_trickplay_item(item_id, |dto| translate::source_of_part(dto, part_id).map(str::to_owned));
        self.get_bytes(&paths::trickplay_sheet(item_id, info.width, sheet, source.as_deref())).ok()
    }

    /// `f` over the item with its thumbnails and versions: the one the playback started from when
    /// it is still held, else read again.
    fn with_trickplay_item<R>(&self, item_id: &str, f: impl Fn(&BaseItemDto) -> Option<R>) -> Option<R> {
        if let Some(held) = self.with_recent(item_id, |dto, _| f(dto)) {
            return held;
        }
        f(&self.get::<BaseItemDto>(&paths::item_trickplay(self.user_id(), item_id))?)
    }

    /// The episode a show's page plays and opens its season on (Plex's `OnDeck`): the one the
    /// viewer is in the middle of, else the next after the last one watched, else the first.
    pub(super) fn on_deck(&self, series_id: &str) -> Option<Metadata> {
        // Both asked at once; the in-progress episode wins when there is one.
        let (resume, next) = both(
            || self.get::<QueryResult<BaseItemDto>>(&paths::resume(self.user_id(), Some(series_id), 0, 1)),
            || self.get::<QueryResult<BaseItemDto>>(&paths::series_next_up(self.user_id(), series_id)),
        );
        let next = next.flatten();
        let from_resume = resume.and_then(|page| page.items.into_iter().find(|i| i.kind == "Episode"));
        let item = from_resume.or_else(|| next?.items.into_iter().next())?;
        translate::metadata(&item, &self.ctx(None))
    }

    /// The queue a playback runs in (`Client::create_play_queue`), shaped as Plex answers one: an
    /// episode and the rest of its show after it when `continuous`, anything else on its own. Up
    /// Next and the end-of-episode hand-off read the row after the selected one.
    pub fn play_queue(&self, item_id: &str, continuous: bool) -> Option<MediaContainer> {
        const WINDOW: i64 = 50;
        // The item the playback just decided on, read seconds ago; its show is its grandparent.
        let me = self.metadata_recent(item_id)?;
        let series = me.grandparent_rating_key.clone();
        let mut rows = match series.as_str() {
            series if continuous && me.kind == "episode" && !series.is_empty() => self
                .get::<QueryResult<BaseItemDto>>(&paths::episodes_from(self.user_id(), series, item_id, WINDOW))
                .map(|page| translate::container(&page, &self.ctx(None)).metadata)
                .filter(|rows| rows.first().is_some_and(|r| r.rating_key == me.rating_key))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if rows.is_empty() {
            rows.push(me);
        }
        for (i, row) in rows.iter_mut().enumerate() {
            row.play_queue_item_id = i as i64 + 1;
        }
        Some(MediaContainer {
            play_queue_id: 1,
            play_queue_selected_item_id: 1,
            play_queue_selected_item_offset: 0,
            play_queue_total_count: rows.len() as i64,
            size: rows.len() as i64,
            metadata: rows,
            ..Default::default()
        })
    }

    /// A show's seasons or a season's episodes (`Client::children_paged`). A season's episodes are
    /// asked for under its show (`parent_rating_key`), because Jellyfin lists episodes by series.
    pub fn children(&self, parent: &Metadata, start: i64, size: i64) -> Option<MediaContainer> {
        let page: QueryResult<BaseItemDto> = match parent.kind.as_str() {
            "show" => self.get(&paths::seasons(self.user_id(), &parent.rating_key))?,
            "season" => self.get(&paths::episodes(
                self.user_id(),
                &parent.parent_rating_key,
                &parent.rating_key,
                start,
                size,
            ))?,
            "collection" => self.get(&paths::items(
                self.user_id(),
                &ItemsQuery {
                    parent_id: Some(&parent.rating_key),
                    sort_by: Some(SortBy::PremiereDate),
                    start_index: start,
                    limit: size,
                    ..Default::default()
                },
            ))?,
            _ => return None,
        };
        Some(translate::container(&page, &self.ctx(None)))
    }

    /// "More like this" (`Client::related`).
    pub fn similar(&self, item_id: &str, size: i64) -> Option<MediaContainer> {
        self.listing(&paths::similar(self.user_id(), item_id, size), None)
    }

    /// Trailers and extras (`Client::extras`).
    pub fn extras(&self, item_id: &str) -> Option<MediaContainer> {
        let features: Option<Vec<BaseItemDto>> = self.get(&paths::special_features(self.user_id(), item_id));
        // Jellyfin keeps trailers apart from the other extras, so they are a read of their own; one
        // that fails leaves the extras, and the reverse.
        let trailers: Option<Vec<BaseItemDto>> = self.get(&paths::local_trailers(self.user_id(), item_id));
        if features.is_none() && trailers.is_none() {
            return None;
        }
        let mut items = trailers.unwrap_or_default();
        items.extend(features.unwrap_or_default());
        Some(translate::container_of(&items, &self.ctx(None)))
    }

    /// Where this server's WebSocket is, signed in as this session (the live-update feed,
    /// [`live`]). `None` when signed out, or when the credential may not go to this origin.
    pub fn socket_target(&self) -> Option<live::Target> {
        let token = self.token()?;
        if !credential_may_go(&self.base) {
            return None;
        }
        Some(live::Target {
            origin: plx_plex::plex::Origin::parse(&self.base)?,
            path: format!("{}/socket", base_path(&self.base)),
            headers: vec![format!("Authorization: {}", self.identity.header(Some(token)))],
            user_id: self.user_id().to_string(),
        })
    }

    /// Make the item a favourite or not (`Client::set_favorite`): the current route, else the
    /// older `/Users/{userId}/FavoriteItems/{itemId}` on a server that answers 404 to it.
    pub fn set_favorite(&self, item_id: &str, favourite: bool) -> bool {
        self.forget_recent();
        let verb = if favourite { Verb::Post } else { Verb::Delete };
        match self.send(verb, &paths::favorite(self.user_id(), item_id), None) {
            Some(a) if a.status == 404 => self
                .send(verb, &paths::favorite_legacy(self.user_id(), item_id), None)
                .is_some_and(|a| a.ok()),
            other => other.is_some_and(|a| a.ok()),
        }
    }

    /// Mark watched (`Client::scrobble`).
    pub fn mark_played(&self, item_id: &str) -> bool {
        self.forget_recent();
        self.send(Verb::Post, &paths::played(self.user_id(), item_id), None).is_some_and(|a| a.ok())
    }

    /// Subtitle candidates for an item from the server's subtitle provider plugins
    /// (`Client::search_subtitles`). `language` is the app's code (`pt`); the server is asked in
    /// its own (`por`). A server without a provider answers an empty list.
    pub fn subtitle_candidates(&self, item_id: &str, language: &str) -> Option<MediaContainer> {
        let found: Vec<RemoteSubtitleInfo> =
            self.get(&paths::remote_subtitles(item_id, &languages::to_jellyfin(language)))?;
        Some(translate::subtitle_candidates(&found))
    }

    /// Download one candidate into the item (`Client::add_subtitle`). The server saves it beside
    /// the file and the item's next read lists it as an external subtitle.
    pub fn download_subtitle(&self, item_id: &str, candidate: &str) -> bool {
        self.forget_recent();
        self.send(Verb::Post, &paths::remote_subtitle(item_id, candidate), None).is_some_and(|a| a.ok())
    }

    /// Mark unwatched (`Client::unscrobble`).
    pub fn mark_unplayed(&self, item_id: &str) -> bool {
        self.forget_recent();
        self.send(Verb::Delete, &paths::played(self.user_id(), item_id), None).is_some_and(|a| a.ok())
    }

    /// Take an item off Continue Watching (`Client::remove_from_continue_watching`). Jellyfin has
    /// no hide flag: clearing the resume point takes the item off Resume, and an episode then
    /// becomes its show's Next Up. So an episode is also recorded as dismissed in this app's
    /// display preferences on the server ([`Self::next_up`] leaves those out), which the user's
    /// other installs share. The watched mark and play count stay as they were.
    pub fn leave_continue_watching(&self, item_id: &str, is_episode: bool) -> bool {
        self.forget_recent();
        let cleared = self
            .send(Verb::Post, &paths::user_data(self.user_id(), item_id), Some(r#"{"PlaybackPositionTicks":0}"#))
            .is_some_and(|a| a.ok());
        cleared && (!is_episode || self.dismiss_next_up(item_id))
    }

    /// Record `item_id` among the Next Up episodes the user dismissed, keeping the last
    /// [`DISMISSED_MAX`] (an episode that was played moves Next Up on and is never named again).
    fn dismiss_next_up(&self, item_id: &str) -> bool {
        // Read anew: the write replaces the whole record, so it starts from the server's.
        let Ok(prefs) = self.display_prefs(Duration::ZERO) else { return false };
        let mut prefs = (*prefs).clone();
        let mut ids = dismissed_in(&prefs);
        ids.retain(|d| d != item_id);
        ids.push(item_id.to_owned());
        ids.drain(..ids.len().saturating_sub(DISMISSED_MAX));
        prefs["CustomPrefs"][DISMISSED_KEY] = ids.join(",").into();
        let written = self.send(Verb::Post, &paths::display_preferences(self.user_id()), Some(&prefs.to_string()))
            .is_some_and(|a| a.ok());
        if written {
            self.recent_prefs.put(Arc::new(prefs));
        }
        written
    }

    /// This app's server-side display preferences, where the dismissed episodes are kept, as read
    /// in the last `fresh_for`.
    fn display_prefs(&self, fresh_for: Duration) -> Result<Arc<serde_json::Value>, ()> {
        self.recent_prefs.get(fresh_for, || {
            self.get::<serde_json::Value>(&paths::display_preferences(self.user_id())).map(Arc::new).ok_or(())
        })
    }

    /// The Next Up episodes the user dismissed; none when the server doesn't answer.
    pub(super) fn dismissed_next_up(&self) -> Vec<String> {
        self.display_prefs(super::RECENT_CONFIG).map(|p| dismissed_in(&p)).unwrap_or_default()
    }

    /// Continue Watching inside one library (a library page's first shelf).
    pub fn continue_watching_in(&self, section_key: i64, start: i64, size: i64) -> Option<MediaContainer> {
        let guid = self.section_guid(section_key)?;
        self.listing(&paths::resume(self.user_id(), Some(&guid), start, size), Some(&guid))
    }

    /// Several items by id, in one request (`Client::metadata_many`).
    pub fn items_by_id(&self, ids: &[&str]) -> Option<MediaContainer> {
        let joined = ids.join(",");
        self.listing(&paths::items(self.user_id(), &ItemsQuery { ids: Some(&joined), ..Default::default() }), None)
    }

    /// The children of an item known only by id (`Client::children_paged`): one light read to
    /// learn whether it is a show, a season or a collection, then its listing.
    pub fn children_of(&self, item_id: &str, start: i64, size: i64) -> Option<MediaContainer> {
        // A page lists its children just after reading the item itself.
        if let Some(parent) = self.with_recent(item_id, |dto, _| translate::metadata(dto, &self.ctx(None))).flatten() {
            return self.children(&parent, start, size);
        }
        let path = paths::items(self.user_id(), &ItemsQuery { ids: Some(item_id), fields: Some("ChildCount"), ..Default::default() });
        let page: QueryResult<BaseItemDto> = self.get(&path)?;
        let parent = translate::metadata(page.items.first()?, &self.ctx(None))?;
        self.children(&parent, start, size)
    }

    /// Raw bytes from a server path (a subtitle file), with the token in the query.
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let a = self
            .send(Verb::Get, &self.with_token(path), None)
            .ok_or_else(|| "no answer".to_string())?;
        if !a.ok() {
            return Err(format!("HTTP {} ({} bytes)", a.status, a.body.len()));
        }
        if a.body.is_empty() {
            return Err("empty body".into());
        }
        Ok(a.body)
    }

    /// Report playback state (`Client::timeline`): the first report of a play session is its
    /// start, a stop ends it, and everything between is progress. Stream indexes of 0 are left out,
    /// as the model uses 0 for "no choice".
    pub fn report(&self, r: &PlaybackReport<'_>) -> bool {
        // The resume point moves; the playback's own steps (a seek's new transcode, an audio
        // switch) still read the item, so it is forgotten when the playback ends, not on progress.
        if r.state == PlayState::Stopped {
            self.forget_recent();
        }
        let mut body = serde_json::json!({
            "ItemId": r.item_id,
            "PositionTicks": ms_to_ticks(r.position_ms),
            "IsPaused": r.state == PlayState::Paused,
            "PlaySessionId": r.play_session_id,
            "PlayMethod": if self.is_transcoding(r.play_session_id) { "Transcode" } else { "DirectPlay" },
            "CanSeek": true,
        });
        if r.audio_index > 0 {
            body["AudioStreamIndex"] = r.audio_index.into();
        }
        if r.subtitle_index > 0 {
            body["SubtitleStreamIndex"] = r.subtitle_index.into();
        }
        let path = {
            let mut started = self.started.lock().unwrap_or_else(|p| p.into_inner());
            match r.state {
                PlayState::Stopped => {
                    started.remove(r.play_session_id);
                    paths::playing_stopped()
                }
                _ if started.insert(r.play_session_id.to_string()) => paths::playing(),
                _ => paths::playing_progress(),
            }
        };
        self.send(Verb::Post, &path, Some(&body.to_string())).is_some_and(|a| a.ok())
    }
}
