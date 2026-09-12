//! Owned Home initial conditions, captured at recording boot. Worker mailboxes are deliberately
//! not drained: those arrivals belong to the recorded first ingest. Client pointers and client
//! credential fields are excluded. Restoring the other stores and binding these logical client ids
//! remain the replay bootstrap's responsibility; this is not a mid-session loader.

use super::*;
use serde::{Deserialize, Serialize};

pub(crate) const SHAPE: &str = "HubsInitialV1{version:u32,generation:u32,next_request:u32,seen:u64,seen_facts:u32,sections_generation:u32,catalog_generation:u32,sources:[{sid:u16,client:Option<u32>,token_gen:u32,handle:str,state:u32,fetching:bool,seq:u32,retry_bits:u32,retry_n:u32,last:Option<SourceBuild>}],catalog:{items:[PmsMovie],hubs:[{title:str,hub_id:str,key:str,source:str,start:u64,len:u64}],heroes:[{idx:u64,source:str}]}}";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Initial {
    #[serde(deserialize_with = "initial_version")]
    version: u32,
    generation: u32,
    next_request: u32,
    seen: u64,
    seen_facts: u32,
    sections_generation: u32,
    catalog_generation: u32,
    sources: Vec<Source>,
    catalog: HomeCatalog,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    #[serde(with = "super::record::server_id")]
    sid: ServerId,
    #[serde(deserialize_with = "required_option")]
    client: Option<u32>,
    token_gen: u32,
    handle: String,
    #[serde(deserialize_with = "source_state")]
    state: u32,
    fetching: bool,
    seq: u32,
    retry_bits: u32,
    retry_n: u32,
    #[serde(deserialize_with = "required_option")]
    last: Option<SourceBuild>,
}

fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

fn initial_version<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let n = u32::deserialize(d)?;
    if n != 1 { return Err(serde::de::Error::custom("unsupported Home initial version")); }
    Ok(n)
}

fn source_state<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let n = u32::deserialize(d)?;
    if n > 2 { return Err(serde::de::Error::custom("invalid Home source state")); }
    Ok(n)
}

/// Data-layer traversal, not a dependency on UI's Canon. The application adapts its canonical
/// writer to these scalar operations. Never hash JSON, host-sized integers, or resource pointers.
pub(crate) trait Sink {
    fn u32(&mut self, v: u32);
    fn u64(&mut self, v: u64);
    fn boolean(&mut self, v: bool);
    fn text(&mut self, v: &str);
}

impl Initial {
    pub(crate) fn snapshot(&self) -> HubsSnapshot {
        HubsSnapshot { data:Arc::new(self.catalog.clone()),generation:self.catalog_generation,state:HubState::Loading }
    }
    #[cfg(any(test, feature = "hostsim"))]
    pub(crate) fn fresh() -> Self {
        Self { version:1,generation:0,next_request:1,seen:u64::MAX,seen_facts:u32::MAX,
            sections_generation:0,catalog_generation:0,sources:Vec::new(),catalog:HomeCatalog::default() }
    }
    pub(crate) fn validate_boot(&self) -> bool {
        self.sources.is_empty() && self.catalog.items.is_empty() && self.catalog.hubs.is_empty()
            && self.catalog.heroes.is_empty()
    }

    /// Restore the validated pre-work inputs, never a mid-flight checkpoint or live mailbox.
    pub(crate) fn restore_boot(&self, _mt: &crate::task::MainThread) -> Result<(), &'static str> {
        if !self.validate_boot() { return Err("Home initial state contains work or content"); }
        HUB_GEN.store(self.generation, Ordering::SeqCst);
        NEXT_REQUEST.store(self.next_request, Ordering::Relaxed);
        SEEN.store(self.seen, Ordering::Relaxed);
        SEEN_FACTS.store(self.seen_facts, Ordering::Relaxed);
        LAST_SECTIONS_GEN.store(self.sections_generation, Ordering::SeqCst);
        CATALOG_GEN.store(self.catalog_generation, Ordering::SeqCst);
        *lock_srcs() = Vec::new();
        unsafe { *std::ptr::addr_of_mut!(PUBLISHED_HOME) = Some(Arc::new(self.catalog.clone())); }
        Ok(())
    }

    pub(crate) fn capture() -> Self {
        let sources = lock_srcs().iter().map(|s| {
            let Src { sid, client, token_gen, handle, state, fetching, seq, retry_s, retry_n, last } = s;
            Source { sid: *sid, client: client.map(|c| c.instance_gen()), token_gen: *token_gen,
                handle: handle.clone(), state: match state { HubState::Loading => 0, HubState::Ready => 1, HubState::Failed => 2 },
                fetching: *fetching, seq: *seq, retry_bits: retry_s.to_bits(), retry_n: *retry_n, last: last.clone() }
        }).collect();
        Self { version: 1, generation: HUB_GEN.load(Ordering::SeqCst),
            next_request: NEXT_REQUEST.load(Ordering::Relaxed), seen: SEEN.load(Ordering::Relaxed),
            seen_facts: SEEN_FACTS.load(Ordering::Relaxed),
            sections_generation: LAST_SECTIONS_GEN.load(Ordering::SeqCst),
            catalog_generation: CATALOG_GEN.load(Ordering::SeqCst), sources,
            catalog: published_home().as_ref().clone() }
    }

    pub(crate) fn write(&self, w: &mut impl Sink) {
        let Self { version, generation, next_request, seen, seen_facts, sections_generation,
            catalog_generation, sources, catalog } = self;
        for n in [version, generation, next_request] { w.u32(*n); }
        w.u64(*seen);
        for n in [seen_facts, sections_generation, catalog_generation] { w.u32(*n); }
        w.u64(sources.len() as u64);
        for s in sources {
            let Source { sid, client, token_gen, handle, state, fetching, seq, retry_bits, retry_n, last } = s;
            w.u32(sid.raw().into()); w.boolean(client.is_some());
            if let Some(id) = client { w.u32(*id); }
            w.u32(*token_gen); w.text(handle); w.u32(*state); w.boolean(*fetching);
            for n in [seq, retry_bits, retry_n] { w.u32(*n); }
            w.boolean(last.is_some());
            if let Some(last) = last { source_build(last, w); }
        }
        let HomeCatalog { items, hubs, heroes } = catalog;
        movies(items, w);
        w.u64(hubs.len() as u64);
        for h in hubs {
            let HubRow { title, hub_id, key, source, start, len } = h;
            for text in [title, hub_id, key, source] { w.text(text); }
            w.u64(*start as u64); w.u64(*len as u64);
        }
        w.u64(heroes.len() as u64);
        for h in heroes { let HeroSlot { idx, source } = h; w.u64(*idx as u64); w.text(source); }
    }
}

fn source_build(b: &SourceBuild, w: &mut impl Sink) {
    let SourceBuild { cw, shelves } = b;
    w.u64(cw.len() as u64);
    for item in cw { let CwItem { last_viewed_at, m } = item; w.u64(*last_viewed_at as u64); movie(m, w); }
    w.u64(shelves.len() as u64);
    for shelf in shelves {
        let Shelf { title, hub_id, key, items } = shelf;
        for text in [title, hub_id, key] { w.text(text); }
        movies(items, w);
    }
}

fn movies(items: &[PmsMovie], w: &mut impl Sink) {
    w.u64(items.len() as u64);
    for m in items { movie(m, w); }
}

fn movie(m: &PmsMovie, w: &mut impl Sink) {
    let PmsMovie { sid, sec, title, year, rating, dur_ns, part, thumb, still, art, summary, rk,
        vcodec, acodec, blur, has_blur, kind, resume_ms, show_rk, season_index, show_title,
        ep_index, unwatched, watched, aired } = m;
    w.u32(sid.raw().into()); w.u64(*sec as u64); w.text(title); w.u32(*year as u32);
    w.text(rating); w.u64(*dur_ns as u64);
    for text in [part, thumb, still, art, summary, rk, vcodec, acodec] { w.text(text); }
    for row in blur { for component in row { w.u32(component.to_bits()); } }
    w.boolean(*has_blur); w.u32(*kind as u32); w.u64(*resume_ms as u64); w.text(show_rk);
    w.u32(*season_index as u32); w.text(show_title); w.u32(*ep_index as u32);
    w.boolean(*unwatched); w.boolean(*watched); w.text(aired);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default, PartialEq, Debug)]
    struct Words(Vec<String>);
    impl Sink for Words {
        fn u32(&mut self, v: u32) { self.0.push(format!("u32:{v}")); }
        fn u64(&mut self, v: u64) { self.0.push(format!("u64:{v}")); }
        fn boolean(&mut self, v: bool) { self.0.push(format!("bool:{v}")); }
        fn text(&mut self, v: &str) { self.0.push(format!("str:{v}")); }
    }
    fn words(init: &Initial) -> Words { let mut w = Words::default(); init.write(&mut w); w }

    #[test]
    fn initial_contents_are_owned_round_trip_and_do_not_consume_arrivals() {
        let _guard = crate::testlock::serial();
        seed_for_test(3, HubState::Ready);
        { let mut s = lock_srcs(); s[0].fetching = true; s[0].seq = 19; s[0].retry_s = -0.0; s[0].retry_n = 4; }
        queue_test_landing(Some(5));
        let init = Initial::capture();
        assert_eq!(take_landings().len(), 1);
        let json = serde_json::to_value(&init).unwrap();
        let decoded: Initial = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(words(&init), words(&decoded));
        assert_eq!(decoded.sources[0].retry_bits, (-0.0f32).to_bits());
        assert_eq!(decoded.catalog.items.len(), 3);
        assert_eq!(decoded.sources[0].last.as_ref().unwrap().shelves[0].items.len(), 3);
        reset();
        assert_eq!(serde_json::to_value(&init).unwrap(), json, "reset cannot mutate a captured initial state");
        assert_ne!(words(&Initial::capture()), words(&init));
    }

    #[test]
    fn canonical_initial_state_includes_hidden_retry_and_request_fields() {
        let _guard = crate::testlock::serial();
        seed_for_test(2, HubState::Ready);
        let initial = Initial::capture();
        let value = serde_json::to_value(&initial).unwrap();
        for field in ["seq", "retry_bits", "retry_n", "token_gen", "state"] {
            let mut changed = value.clone();
            changed["sources"][0][field] = serde_json::json!(if field == "state" { 2 } else { 123 });
            let changed: Initial = serde_json::from_value(changed).unwrap();
            assert_ne!(words(&initial), words(&changed), "{field}");
        }
        let mut bad = value.clone(); bad["version"] = serde_json::json!(2);
        assert!(serde_json::from_value::<Initial>(bad).is_err());
        let mut bad = value; bad["sources"][0]["state"] = serde_json::json!(3);
        assert!(serde_json::from_value::<Initial>(bad).is_err());
        reset();
    }
}
