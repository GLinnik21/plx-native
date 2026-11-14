//! App-owned cardspeed capture, retained across presents; render walks receive only a value.
use plx_ui::card_motion::{AdmissionPolicy, PolicyScope};

#[derive(Default)]
pub(crate) struct Capture(Option<AdmissionPolicy>);
impl Capture {
    pub(crate) fn neutral() -> Self { Self(Some(AdmissionPolicy::default())) }

    pub(crate) fn enter(&mut self, controlled: bool) -> PolicyScope {
        self.enter_with(controlled, || plx_base::devtrig::read("cardspeed"))
    }

    fn enter_with(&mut self, controlled: bool, read: impl FnOnce() -> Option<String>) -> PolicyScope {
        plx_base::step::assert_io_allowed("capture card motion policy");
        // Controlled rendering has an explicit neutral policy, including after a live owner ran.
        // Never invoke the ambient reader on this path or inherit an enclosing live draw's policy.
        let policy = if controlled { AdmissionPolicy::default() } else {
            *self.0.get_or_insert_with(|| {
                let text = read();
                let policy = AdmissionPolicy::from_trigger(text.as_deref());
                if let Some(text) = text {
                    plx_base::eventlog::log(&format!("card_motion: captured cardspeed {:?}: {policy:?}", text.trim()));
                }
                policy
            })
        };
        policy.enter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn moving_declines() -> bool {
        let _moving = plx_ui::card_motion::Scope::moving_for_test();
        plx_ui::card_motion::declines_request()
    }
    #[test]
    fn independent_hosts_latch_once_and_controlled_never_reads_or_inherits_live_policy() {
        let mut live = Capture::default();
        let mut other = Capture::default();
        let mut controlled = Capture::default();
        {
            let _live = live.enter_with(false, || Some("480".into()));
            assert!(moving_declines());
            {
                let _controlled = controlled.enter_with(true, || panic!("controlled ambient read"));
                assert!(!moving_declines());
            }
            assert!(moving_declines(), "controlled nested draw restores live owner");
            {
                let _other = other.enter_with(false, || None);
                assert!(!moving_declines());
            }
            assert!(moving_declines(), "independent neutral owner restores live owner");
        }
        assert!(!moving_declines(), "draw completion restores neutral policy");
        let _again = live.enter_with(false, || panic!("policy must remain latched"));
        assert!(moving_declines());
        assert!(controlled.0.is_none(), "controlled never captures a live fallback");
    }
    #[test]
    fn absent_and_invalid_captures_stay_latched() {
        {
            let mut fixture = Capture::neutral();
            let _explicit = fixture.enter_with(false, || panic!("explicit fixture must not read ambient policy"));
            assert!(!moving_declines());
        }
        for text in [None, Some("invalid"), Some("NaN"), Some("0")] {
            let mut capture = Capture::default();
            { let _first = capture.enter_with(false, || text.map(str::to_owned)); assert!(!moving_declines()); }
            let _again = capture.enter_with(false, || panic!("even neutral policy is latched"));
            assert!(!moving_declines());
        }
    }
}
