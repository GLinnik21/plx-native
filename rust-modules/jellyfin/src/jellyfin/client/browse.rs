//! Libraries, Home's shelves, library grids, people and search.

use super::*;

impl JfClient {
    // ---- libraries and home ----

    /// The user's libraries (`Client::sections`).
    pub fn libraries(&self) -> Option<MediaContainer> {
        self.libraries_recent(Duration::ZERO)
    }

    /// The user's libraries as listed in the last `fresh_for`, else listed now.
    pub fn libraries_recent(&self, fresh_for: Duration) -> Option<MediaContainer> {
        let views = self.recent_libraries.get(fresh_for, || {
            self.get::<QueryResult<BaseItemDto>>(&paths::user_views(self.user_id())).map(Arc::new).ok_or(())
        }).ok()?;
        Some(translate::sections(&views, &self.sections))
    }

    /// A library's model type (`movie`, `show`, ...) from the last listing of the libraries,
    /// listing them again when that one does not have it (or there was none).
    pub fn library_kind(&self, section_key: i64) -> Option<String> {
        let kind_in = |c: MediaContainer| c.directory.into_iter().find(|d| d.key.parse() == Ok(section_key)).map(|d| d.kind);
        self.libraries_recent(Duration::MAX).and_then(kind_in).or_else(|| self.libraries_recent(FRESH).and_then(kind_in))
    }


    /// Continue Watching (`Client::continue_watching_page`).
    pub fn continue_watching(&self, start: i64, size: i64) -> Option<MediaContainer> {
        self.listing(&paths::resume(self.user_id(), None, start, size), None)
    }

    /// Next Up across every show, without the episodes the user dismissed
    /// ([`Self::leave_continue_watching`]); the list is read beside the page, so it costs no wait.
    /// The server pages the list WITH the dismissed episodes, so the read starts at the top and
    /// reaches as far past the window as there can be dismissed episodes, and the window, its
    /// offset and the total are cut from what is left.
    pub fn next_up(&self, start: i64, size: i64) -> Option<MediaContainer> {
        let start = start.max(0);
        let reach = if size > 0 { start + size + super::items::DISMISSED_MAX as i64 } else { size };
        let (page, dismissed) = both(|| self.listing(&paths::next_up(self.user_id(), 0, reach), None), || self.dismissed_next_up());
        let mut page = page?;
        let dismissed = dismissed.unwrap_or_default();
        let listed = page.metadata.len();
        page.metadata.retain(|m| !dismissed.contains(&m.rating_key));
        page.total_size = (page.total_size - (listed - page.metadata.len()) as i64).max(page.metadata.len() as i64);
        page.metadata = page.metadata.into_iter().skip(start as usize).take(if size > 0 { size as usize } else { usize::MAX }).collect();
        page.size = page.metadata.len() as i64;
        page.offset = start;
        Some(page)
    }

    /// The movies, shows and episodes the user marked as favourites, by name.
    pub fn favorites(&self, start: i64, size: i64) -> Option<MediaContainer> {
        let path = paths::items(
            self.user_id(),
            &ItemsQuery {
                include_item_types: Some("Movie,Series,Episode"),
                recursive: true,
                sort_by: Some(SortBy::Title),
                filters: Some("IsFavorite"),
                start_index: start,
                limit: size,
                ..Default::default()
            },
        );
        self.listing(&path, None)
    }

    /// The Home keys of the user's `Configuration`; the defaults when the server doesn't answer, so
    /// Home still draws.
    /// Home reads them [`FRESH`]; a library page's shelves take them as Home last read them, up to
    /// [`RECENT_CONFIG`] ago.
    fn home_config(&self, fresh_for: Duration) -> UserHomeConfig {
        self.user_configuration(fresh_for).ok().and_then(|c| UserHomeConfig::deserialize(&c).ok()).unwrap_or_default()
    }

    /// Recently added in one library, leaving out what the user has watched if they ask Jellyfin to.
    pub fn latest(&self, section_key: i64, size: i64) -> Option<MediaContainer> {
        self.latest_hiding(section_key, size, self.home_config(RECENT_CONFIG).hide_played_in_latest == Some(true))
    }

    fn latest_hiding(&self, section_key: i64, size: i64, hide_played: bool) -> Option<MediaContainer> {
        let guid = self.section_guid(section_key)?;
        let items: Vec<BaseItemDto> = self.get(&paths::latest(self.user_id(), &guid, size, hide_played))?;
        Some(translate::container_of(&items, &self.ctx(Some(&guid))))
    }

    /// The home screen's shelves (`Client::home_hubs`): Favorites, then Recently Added for each movie and TV library, in the order the user arranged their views
    /// (`OrderedViews`), without the libraries they excluded from Latest (`LatestItemsExcludes`) and,
    /// if they hide watched content (`HidePlayedInLatest`), without what they've watched. A shelf that comes back
    /// empty is left out; one whose request fails is left out too, so one slow library doesn't
    /// blank the whole screen.
    ///
    /// The shelves are asked for at once rather than one after another: on a slow server each
    /// request is most of a second, and Home waits for all of them. Favorites go out beside the
    /// library list and the user's settings; the per-library shelves go out as soon as those two
    /// answer. Continue Watching and Next Up are not here: Home draws that row from its own deck
    /// (`Client::continue_watching`) and drops a hub list's copy of it, so asking for it twice would
    /// only crowd the server while Home waits.
    pub fn home_hubs(&self) -> Option<MediaContainer> {
        let (recent, favorites) = both(|| self.recently_added_shelves(), || self.favorites(0, HOME_SHELF_SIZE));
        let (recent, favorites) = (recent?, favorites.flatten());
        let mut hubs: Vec<Hub> = Vec::new();
        if let Some(c) = favorites {
            hubs.push(translate::hub("home.favorites", msg::browse_jf_favorites(), "/jf/favorites", c));
        }
        hubs.extend(recent);
        Some(translate::hubs(hubs))
    }

    /// Home's Recently Added shelves: the library list and the user's settings together, then every
    /// library's shelf at once. `None` when the library list does not answer.
    fn recently_added_shelves(&self) -> Option<Vec<Hub>> {
        let (libs, config) = both(|| self.libraries_recent(FRESH), || self.home_config(FRESH));
        let (libs, config) = (libs?, config.unwrap_or_default());
        let hide_played = config.hide_played_in_latest == Some(true);
        let excluded: HashSet<String> = config.latest_items_excludes.iter().map(|g| normalize(g)).collect();
        let rank = |lib: &LibrarySection| {
            let guid = self.sections.guid(lib.key.parse().unwrap_or_default()).unwrap_or_default();
            config.ordered_views.iter().position(|g| normalize(g) == guid).unwrap_or(usize::MAX)
        };
        let mut ordered: Vec<&LibrarySection> = libs.directory.iter().collect();
        ordered.sort_by_key(|lib| rank(lib));
        let shelves: Vec<(&str, i64, &str)> = ordered
            .into_iter()
            .filter(|lib| !excluded.contains(&self.sections.guid(lib.key.parse().unwrap_or_default()).unwrap_or_default()))
            .filter_map(|lib| {
                let id = match lib.kind.as_str() {
                    "movie" => "home.movies.recent",
                    "show" => "home.television.recent",
                    _ => return None,
                };
                Some((id, lib.key.parse::<i64>().ok()?, lib.title.as_str()))
            })
            .collect();
        let latest: Vec<Option<MediaContainer>> = std::thread::scope(|s| {
            let asked: Vec<_> = shelves.iter().map(|&(_, key, _)| s.spawn(move || self.latest_hiding(key, HOME_SHELF_SIZE, hide_played))).collect();
            asked.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        let hubs = shelves
            .into_iter()
            .zip(latest)
            .filter_map(|((id, key, title), c)| {
                let title = msg::browse_home_hub_recently_added_in(title);
                Some(translate::hub(id, &title, &format!("/jf/latest/{key}"), c?))
            })
            .collect();
        Some(hubs)
    }

    /// A library's grid, one page (`Client::section_items_query`). Movies list movies; TV
    /// libraries list shows.
    pub fn library_items(&self, section_key: i64, q: &LibraryPage<'_>) -> Option<MediaContainer> {
        let guid = self.section_guid(section_key)?;
        // A collection (BoxSet) lives in the server's own collections folder, never under the
        // library its members are in, so collections are listed server-wide.
        let collections = q.item_types == "BoxSet";
        let path = paths::items(
            self.user_id(),
            &ItemsQuery {
                parent_id: (!collections).then_some(guid.as_str()),
                include_item_types: Some(q.item_types),
                recursive: true,
                sort_by: Some(q.sort_by),
                descending: q.descending,
                start_index: q.start,
                limit: q.size,
                // Plex's `Size=0` is a count probe; here a zero limit would list everything.
                count_only: q.size == 0,
                filters: if q.unwatched_only { Some("IsUnplayed") } else { None },
                name_starts_with: q.starts_with,
                genre_ids: q.genre_id,
                ..Default::default()
            },
        );
        self.listing(&path, Some(&guid))
    }

    /// The genres a library's grid can be narrowed to (`Client::section_directory`, `genre`): a
    /// `directory` row per genre, its Jellyfin id as the key.
    pub fn library_genres(&self, section_key: i64, item_types: &str) -> Option<MediaContainer> {
        let guid = self.section_guid(section_key)?;
        let page: QueryResult<BaseItemDto> = self.get(&paths::genres(self.user_id(), &guid, item_types))?;
        Some(translate::genre_rows(&page))
    }

    /// Every film and show a person appears in, across every library, newest first
    /// (`Client::person_media`). `person_id` is the person's id, the cast row's `tag_key`.
    pub fn person_media(&self, person_id: &str) -> Option<MediaContainer> {
        const MOST: i64 = 200;
        let path = paths::items(
            self.user_id(),
            &ItemsQuery {
                person_ids: Some(person_id),
                include_item_types: Some("Movie,Series"),
                recursive: true,
                sort_by: Some(paths::SortBy::PremiereDate),
                descending: true,
                limit: MOST,
                ..Default::default()
            },
        );
        self.listing(&path, None)
    }

    /// A person's biography (`AccountClient::person_profile`): the overview, birth and death
    /// dates and birthplace Jellyfin keeps on the person item. A person the server does not know
    /// is an empty profile, the answer the page settles on.
    pub fn person_profile(&self, person_id: &str) -> Option<plx_plex::plex::discover::PersonProfile> {
        let answer = self.send(Verb::Get, &paths::item(self.user_id(), person_id), None)?;
        if answer.status == 404 {
            return Some(Default::default());
        }
        if !answer.ok() {
            return None;
        }
        let i: BaseItemDto = serde_json::from_slice(&answer.body).ok()?;
        Some(translate::person_profile(&i))
    }

    /// People matching a search (`Client::search_typed` for the People tab).
    pub fn search_people(&self, query: &str, start: i64, size: i64) -> Option<MediaContainer> {
        let page: QueryResult<BaseItemDto> = self.get(&paths::persons(self.user_id(), query, start, size))?;
        Some(translate::people_hits(&page))
    }

    /// The People shelf of a search: Plex's `actor` hub, whose rows are directory tags.
    pub fn people_hub(&self, query: &str, limit: i64) -> Option<plx_plex::plex::Hub> {
        let found = self.search_people(query, 0, limit)?;
        Some(plx_plex::plex::Hub {
            hub_identifier: "actor".into(),
            kind: "actor".into(),
            size: found.size,
            total_size: found.total_size,
            more: found.total_size > found.size,
            directory: found.search_result.into_iter().filter_map(|r| r.directory).collect(),
            ..Default::default()
        })
    }

    /// Search across every library (`Client::search_typed`).
    pub fn search(&self, query: &str, item_types: &str, start: i64, size: i64) -> Option<MediaContainer> {
        let path = paths::items(
            self.user_id(),
            &ItemsQuery {
                search_term: Some(query),
                include_item_types: Some(item_types),
                recursive: true,
                start_index: start,
                limit: size,
                ..Default::default()
            },
        );
        self.listing(&path, None)
    }
}
