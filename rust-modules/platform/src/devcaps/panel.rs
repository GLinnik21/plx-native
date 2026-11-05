use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Unknown,
    NotUhd,
    Uhd,
}

impl Panel {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NotUhd => "not-uhd",
            Self::Uhd => "uhd",
        }
    }

    pub const fn max_height(self) -> Option<u32> {
        match self {
            Self::Unknown => None,
            Self::NotUhd => Some(1080),
            Self::Uhd => Some(2160),
        }
    }
}

struct PanelCache(OnceLock<Panel>);

impl PanelCache {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self) -> Panel {
        self.0.get().copied().unwrap_or(Panel::Unknown)
    }

    fn publish(&self, panel: Panel) {
        let _ = self.0.set(panel);
    }
}

static RESULT: PanelCache = PanelCache::new();

pub fn panel() -> Panel {
    RESULT.get()
}

pub fn publish(panel: Panel) {
    RESULT.publish(panel);
}

#[cfg(test)]
mod tests {
    use super::{Panel, PanelCache};

    #[test]
    fn an_unpublished_panel_is_unknown_and_the_first_answer_stays() {
        let cache = PanelCache::new();
        assert_eq!(cache.get(), Panel::Unknown);
        cache.publish(Panel::NotUhd);
        cache.publish(Panel::Uhd);
        assert_eq!(cache.get(), Panel::NotUhd);
    }

    #[test]
    fn only_a_known_panel_bounds_the_version_height() {
        assert_eq!(Panel::Unknown.max_height(), None);
        assert_eq!(Panel::NotUhd.max_height(), Some(1080));
        assert_eq!(Panel::Uhd.max_height(), Some(2160));
    }
}
