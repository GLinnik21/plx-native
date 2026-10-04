// Keep mock workers inside an explicit test module for the production dependency gate.
#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn compound_show_sort_reverses_only_the_show_when_the_descending_key_is_absent() {
        let mut sort = SortEntry {
            key: "show.titleSort,season.index:nullsLast,episode.index:nullsLast,episode.id".into(),
            desc_key: String::new(), title: "Show".into(), default_desc: false,
        };
        assert_eq!(sort.query(false), sort.key);
        assert_eq!(sort.query(true),
            "show.titleSort:desc,season.index:nullsLast,episode.index:nullsLast,episode.id");
        sort.desc_key = "show.titleSort:desc,episode.id".into();
        assert_eq!(sort.query(true), sort.desc_key, "the server's expression takes precedence");
        sort.key = "addedAt".into();
        sort.desc_key.clear();
        assert_eq!(sort.query(false), "addedAt:asc", "simple sorts retain an explicit direction");
        assert_eq!(sort.query(true), "addedAt:desc");
    }

    #[test]
    fn tv_library_type_changes_query_and_preserves_each_sections_choice() {
        let _guard = plx_base::testlock::serial();
        let mut state = BrowseState::default();
        seed_two_source_table_for_owner_test(&mut state);
        let adapter = Arc::new(BrowseAdapter::default());
        state.set_cur(1);
        let target = crate::stores::browse::SectionAddress {
            epoch: state.table_epoch(), sid: state.section_sid(1).unwrap(), section: 2,
        };
        let old = state.listing_snapshot();
        let current = &mut state.states[1];
        current.total = 1;
        current.items = SecItems::from_vec(vec![Some(PmsMovie::default())]);
        current.sorts = Arc::new(vec![SortEntry {
            desc_key: String::new(),
            key: "addedAt".into(), title: "Date Added".into(), default_desc: true,
        }]);
        current.sort_desc = true;
        current.genre = Some(Arc::new(GenreEntry { id: "7".into(), title: "Drama".into() }));
        current.genres_done = true;
        current.letters = Arc::new(vec![("A".into(), 1), ("B".into(), 1)]);
        current.letters_done = true;
        current.cursor = Some(Arc::new(Cursor { at: CursorAt::SlotIndex(0), scroll: 12.0 }));
        current.unwatched = true;

        assert!(state.addressed_with_adapter(&adapter, target, crate::stores::browse::LibraryWork::Commit {
            select: false, choice: false,
            query: Some(crate::stores::browse::QueryEdit::LibraryType(LibraryType::Episodes)),
        }));
        let listing = state.listing_snapshot();
        assert_eq!(listing.view().library_type(), LibraryType::Episodes);
        assert_eq!(old.view().library_type(), LibraryType::Primary, "retained publication stays immutable");
        assert_ne!(listing.view().id().unwrap().query, old.view().id().unwrap().query);
        assert_eq!(listing.view().total(), -1);
        assert!(listing.view().item(0).is_none());
        assert!(listing.view().sorts().is_empty());
        assert!(!listing.view().sort_desc());
        assert!(listing.view().genre().is_none());
        assert!(listing.view().letters().is_empty());
        assert!(listing.view().cursor().is_none());
        assert!(listing.view().unwatched());
        assert!(!state.states[1].genres_done && !state.states[1].letters_done);
        assert!(!state.set_genre_by_id(Some("7")));
        assert_eq!(state.states[1].query_filters(SecKind::Show),
            vec![("type".into(), "4".into()), ("unwatched".into(), "1".into())]);

        state.set_cur(3);
        assert_eq!(state.listing_snapshot().view().library_type(), LibraryType::Primary);
        assert!(state.set_library_type(LibraryType::Seasons));
        state.set_unwatched(true);
        assert_eq!(state.states[3].query_filters(SecKind::Show),
            vec![("type".into(), "3".into()), ("unwatchedLeaves".into(), "1".into())]);
        state.set_cur(1);
        assert_eq!(state.listing_snapshot().view().library_type(), LibraryType::Episodes);
        let gen = state.query_gen();
        assert!(state.set_library_type(LibraryType::Episodes));
        assert_eq!(state.query_gen(), gen, "reselecting the current type keeps the loaded listing");

        state.set_cur(0);
        let gen = state.query_gen();
        assert!(!state.set_library_type(LibraryType::Episodes));
        assert_eq!(state.query_gen(), gen);
        state.set_unwatched(true);
        assert_eq!(state.states[0].query_filters(SecKind::Movie), vec![("unwatched".into(), "1".into())]);
    }

    /// **Design A: a movie library lists its collections from the TYPE menu.** The listing asks
    /// for `type=18` (and `firstCharacter?type=18` for its letters, via `plex_type`), sends neither
    /// filter — a collection has no watch state or genre — and shows neither as applied. Unwatched
    /// is the section's own switch and comes back with its films; a genre belongs to one metadata
    /// type's menu, so a type change drops it, as it always has between TV types.
    #[test]
    fn a_movie_library_lists_its_collections_without_filters() {
        let _guard = plx_base::testlock::serial();
        assert_eq!(LibraryType::offered(SecKind::Movie), [LibraryType::Primary, LibraryType::Collections]);
        assert_eq!(LibraryType::offered(SecKind::Show),
            [LibraryType::Primary, LibraryType::Seasons, LibraryType::Episodes, LibraryType::Collections]);
        assert_eq!(LibraryType::Collections.plex_type(SecKind::Movie), Some(18));
        assert_eq!(LibraryType::Collections.plex_type(SecKind::Show), Some(18));
        assert_eq!(LibraryType::Primary.plex_type(SecKind::Movie), None, "films are still asked for untyped");

        let mut state = BrowseState::default();
        seed_two_source_table_for_owner_test(&mut state);
        state.set_cur(0);
        assert_eq!(state.sections[0].kind, SecKind::Movie);
        let gen = state.query_gen();
        assert!(!state.set_library_type(LibraryType::Seasons), "a movie library has no seasons");
        assert_eq!(state.query_gen(), gen);
        state.set_unwatched(true);
        state.states[0].genre = Some(Arc::new(GenreEntry { id: "7".into(), title: "Drama".into() }));
        assert!(state.set_library_type(LibraryType::Collections));
        assert_ne!(state.query_gen(), gen);
        assert_eq!(state.states[0].query_filters(SecKind::Movie), vec![("type".into(), "18".into())]);
        let listing = state.listing_snapshot();
        assert_eq!(listing.view().library_type(), LibraryType::Collections);
        assert!(!listing.view().unwatched() && listing.view().genre().is_none(),
            "neither filter reads as applied to a collections listing");
        assert!(!state.set_genre_by_id(Some("7")), "genres are the primary listing's");

        assert!(state.set_library_type(LibraryType::Primary));
        assert_eq!(state.states[0].query_filters(SecKind::Movie),
            vec![("unwatched".into(), "1".into())],
            "the films' Unwatched switch comes back with them");
    }

    #[test]
    fn tv_library_type_discards_directories_fetched_for_another_type() {
        let _guard = plx_base::testlock::serial();
        let (_cleanup, mut browse, _, client) = test_support::registered_page_source();
        browse.state.sections[0].kind = SecKind::Show;
        let captured_type = browse.state.states[0].library_type;
        assert!(browse.state.set_library_type(LibraryType::Episodes));
        let mail = Mutex::new(Some(DirectoryResult {
            epoch: browse.state.table_epoch(), sec: 0, client, token_gen: client.token_gen(),
            library_type: captured_type, list: vec![("S".into(), 99)],
        }));
        let fetching = AtomicBool::new(true);
        assert!(!browse.state.land_directory_owned(&fetching, &mail, |state, list| {
            state.letters = Arc::new(list);
            state.letters_done = true;
        }));
        assert!(!fetching.load(Ordering::SeqCst));
        assert!(browse.state.states[0].letters.is_empty());
        assert!(!browse.state.states[0].letters_done);
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn tv_library_type_scopes_wire_pages_and_letters_and_confirms_the_active_sort() {
        use std::io::{BufRead, BufReader, Write};

        let _guard = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let _cleanup = test_support::RegisteredCleanup;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as i32;
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            for body in [
                r#"{"MediaContainer":{"totalSize":121,"Metadata":[{"ratingKey":"unsorted"}],"Meta":{"Type":[{"active":false,"Sort":[{"key":"titleSort","title":"Title"}]},{"active":true,"Sort":[{"key":"show.titleSort,episode.index","descKey":"show.titleSort:desc,episode.index","title":"Show","defaultDirection":"asc"}]}]}}}"#,
                r#"{"MediaContainer":{"totalSize":121,"Metadata":[{"type":"episode","ratingKey":"11","title":"Episode","thumb":"/episode/still","grandparentThumb":"/show/poster"}]}}"#,
                r#"{"MediaContainer":{"totalSize":121,"Metadata":[{"type":"episode","ratingKey":"71"}]}}"#,
                r#"{"MediaContainer":{"totalSize":121,"Metadata":[{"type":"episode","ratingKey":"91"}]}}"#,
                r#"{"MediaContainer":{"Directory":[{"key":"A","title":"A","size":121}]}}"#,
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&socket).read_line(&mut request).unwrap();
                tx.send(request).unwrap();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let sid = plx_plex::plex::register_for_test("library-type-fixture", "127.0.0.1", port, "", "fixture");
        let client = plx_plex::plex::client_for(sid).unwrap();
        let state = SecState { library_type: LibraryType::Episodes, unwatched: true, ..Default::default() };
        let filters = state.query_filters(SecKind::Show);
        let first = SectionQuery { section_key: 2, sort: "", filters: &filters, start: 0, size: 60, include_meta: true };
        let ListingPage { items, total, sorts, .. } = fetch_listing_page(client, sid, &first, true, None);
        assert_eq!(total, 121);
        assert_eq!(items[0].rk, "11", "the unsorted discovery page must never be published");
        assert_eq!(items[0].still, "/episode/still");
        assert_eq!(items[0].thumb, "/show/poster");
        let sorts = sorts.unwrap();
        assert_eq!(sorts[0].key, "show.titleSort,episode.index");
        let ascending = sorts[0].query(false);
        let next = SectionQuery { start: 60, sort: &ascending, include_meta: false, ..first };
        let ListingPage { items, total, .. } = fetch_listing_page(client, sid, &next, true, None);
        assert_eq!(items[0].rk, "71");
        assert_eq!(total, 121);
        let descending = sorts[0].query(true);
        assert_eq!(descending, "show.titleSort:desc,episode.index");
        let reversed = SectionQuery { start: 0, sort: &descending, ..next };
        assert_eq!(fetch_listing_page(client, sid, &reversed, true, None).items[0].rk, "91");
        assert_eq!(client.section_directory(2, "firstCharacter", Some(4)).unwrap().directory[0].size, 121);
        let requests: Vec<String> = (0..5).map(|_| rx.recv().unwrap()).collect();
        for request in &requests[..4] {
            assert!(request.contains("type=4"), "{request}");
            assert!(request.contains("unwatched=1"), "{request}");
            assert!(!request.contains("unwatchedLeaves"), "{request}");
            assert!(request.contains("X-Plex-Container-Size=60"), "{request}");
        }
        assert!(requests[0].contains("includeMeta=1"));
        assert!(requests[0].contains("X-Plex-Container-Start=0"));
        assert!(!requests[1].contains("includeMeta=1"));
        assert!(requests[1].contains("sort=show.titleSort%2Cepisode.index&"));
        assert!(requests[2].contains("sort=show.titleSort%2Cepisode.index&"));
        assert!(requests[2].contains("X-Plex-Container-Start=60"));
        assert!(requests[3].contains("sort=show.titleSort%3Adesc%2Cepisode.index&"));
        assert!(requests[4].contains("/library/sections/2/firstCharacter?type=4"));
        server.join().unwrap();
    }
}
