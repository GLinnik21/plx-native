//! Entry-owned restoration data; current focus remains in the container's ReturnState.
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Memory {
    pub(super) profile: u32,
    pub(super) query: u32,
    pub(super) next_elem: u32,
    pub(super) keys: Vec<KeyEntry>,
    pub(super) scroll: f32,
    pub(super) rows: Vec<(Kind, f32)>,
}

impl Memory {
    pub(crate) const SHAPE: &'static str = "PageMemory::Search(tag=6,SearchMemory);SearchMemory{profile:u32,query:u32,next_elem:u32,keys:[SearchKey{identity:{Recent(str),Media(kind:u32,sid:u32,rk:str),Tag(kind:u32,sid:u32,id:str),Slot(kind:u32,query:u32,index:u64)},elem:u32,group:u32,slot:u64}],scroll:f32,rows:[(kind:u32,scroll:f32)]}";
}

impl LogicalState for Memory {
    fn write(&self, c: &mut Canon) {
        c.u32(self.profile).u32(self.query).u32(self.next_elem).f32(self.scroll);
        c.seq(self.keys.len());
        for key in &self.keys { key.write(c); }
        c.seq(self.rows.len());
        for (kind, scroll) in &self.rows { c.u32(layout::ordinal(*kind)).f32(*scroll); }
    }
    fn probe(&self, out: &mut String) { out.push_str("search_memory"); }
}

impl SearchScreen {
    pub(crate) fn restore(&mut self, memory: &Memory) {
        self.restored = Some(memory.clone());
        self.content_dirty = true;
    }
    pub(super) fn page_memory(&self) -> Memory {
        Memory { profile: self.draft.profile(), query: self.query_gen, next_elem: self.next_elem,
            keys: self.keys.clone(), scroll: self.scroll.pos,
            rows: self.rows.iter().map(|row| (row.kind, row.motion.scroll_x())).collect() }
    }
}
