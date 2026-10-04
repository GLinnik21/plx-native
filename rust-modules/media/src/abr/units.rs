#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MediaTimeMs(pub i64);

impl MediaTimeMs {
    pub fn saturating_since(self, earlier: Self) -> i64 {
        self.0.saturating_sub(earlier.0).max(0)
    }
}
