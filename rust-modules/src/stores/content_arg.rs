//! The identity of a content page (a detail, a person, a filmography, a collection), as the
//! navigation entry carries it. It is defined here, in the data layer, because a search hit names
//! the page it opens (`search::CollectionHit::route`); `screens::registry` re-exports it for the
//! screens and the app, which spell it `registry::ContentArg`.

/// An item's or person's identity travels with the navigation entry, never in a screen global.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContentArg {
    Detail { sid: crate::plex::ServerId, rk: String },
    Person { sid: crate::plex::ServerId, key: String, guid: String, name: String, thumb: String },
    Filmography { sid: crate::plex::ServerId, key: String },
    Collection(crate::plex::collections::CollectionRef),
}

impl crate::ui::machine::LogicalState for ContentArg {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        match self {
            Self::Detail { sid, rk } => { c.u32(0).u32(u32::from(sid.raw())).str(rk); }
            Self::Person { sid, key, guid, name, thumb } => { c.u32(1).u32(u32::from(sid.raw())).str(key).str(guid).str(name).str(thumb); }
            Self::Filmography { sid, key } => { c.u32(2).u32(u32::from(sid.raw())).str(key); }
            Self::Collection(id) => {
                c.u32(3).u32(u32::from(id.sid.raw())).str(&id.rk).u64(id.sec as u64).u64(id.tag as u64).str(&id.name);
            }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("content_arg"); }
}

impl ContentArg {
    pub(crate) fn same_item(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Detail { sid: a, rk: x }, Self::Detail { sid: b, rk: y }) =>
                a == b && x == y,
            (Self::Person { sid: a, key: x, guid: g, .. },
             Self::Person { sid: b, key: y, guid: h, .. }) =>
                if !g.is_empty() && !h.is_empty() { g == h } else { a == b && x == y },
            (Self::Filmography { sid: a, key: x }, Self::Filmography { sid: b, key: y }) =>
                a == b && x == y,
            (Self::Collection(a), Self::Collection(b)) => a.same_collection(b),
            _ => false,
        }
    }
}
