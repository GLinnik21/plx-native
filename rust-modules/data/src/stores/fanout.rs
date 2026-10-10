//! The gate on work that starts once per server.
//!
//! A store that asks every server the same question (Search, a Person page, Home's hubs) used to be
//! bounded by the registry's old sixteen slots. With the registry unbounded, forty servers would
//! mean forty requests at once, each on its own worker. [`Fanout`] lets at most [`WIDTH`] be out at a
//! time and hands the next grant out as each one finishes, so forty servers cost forty requests
//! over time and never forty at once.
//!
//! **Fair, and nothing is dropped.** A server that is not granted this pump is simply still wanted:
//! the store asks again next pump, because what makes it wanted (no answer yet, a retry that is due)
//! has not changed. The grant order is round-robin from where the last grant ended, so no server
//! waits behind the same early neighbours pump after pump.
//!
//! **Under [`WIDTH`] servers it changes nothing.** A server is counted once, either running or
//! wanting, so with at most [`WIDTH`] servers every wanting one is granted in the same pump, in the
//! store's own order. The recorded landings of existing fixtures (rosters of four or fewer) replay
//! identically.

/// Most requests one store has out at once across its servers.
pub const WIDTH: usize = 4;

/// One store's fairness cursor: the fetch id the last grant ended on.
#[derive(Default)]
pub struct Fanout {
    cursor: usize,
    started: bool,
}

/// One pump's grants. Built by [`Fanout::begin`]; the store asks [`FanoutTurn::admit`] at the point
/// it would start a request.
pub struct FanoutTurn {
    running: usize,
    reserved: Vec<usize>,
}

impl Fanout {
    /// Open a pump. `running` counts requests already out; `wanting` lists, in the store's order,
    /// the ids that would start one now. Up to the room left, the ones after the cursor are
    /// reserved first, wrapping to the front.
    pub fn begin(&mut self, running: usize, wanting: &[usize]) -> FanoutTurn {
        let room = WIDTH.saturating_sub(running);
        let mut reserved = Vec::new();
        if wanting.len() <= room {
            reserved.extend_from_slice(wanting);
        } else if room > 0 {
            let after = wanting.iter().position(|&id| self.started && id > self.cursor).unwrap_or(0);
            reserved.extend(wanting.iter().cycle().skip(after).take(room).copied());
        }
        if let Some(&last) = reserved.last() {
            self.cursor = last;
            self.started = true;
        }
        FanoutTurn { running, reserved }
    }
}

impl FanoutTurn {
    /// A request that was out finished this pump, freeing its place for a start later in the pump.
    pub fn finished(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    /// May `id` start a request now? A reserved id always may. One that was not wanted when the pump
    /// opened (an answer this pump made it wanted) may use room nobody reserved.
    pub fn admit(&mut self, id: usize) -> bool {
        if let Some(at) = self.reserved.iter().position(|&r| r == id) {
            self.reserved.swap_remove(at);
            self.running += 1;
            return true;
        }
        if self.running + self.reserved.len() < WIDTH {
            self.running += 1;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run pumps over `n` servers that each want one request until it has run for `pumps_to_finish`
    /// pumps; returns the most ever out at once and the order servers were granted in.
    fn run(n: usize, pumps_to_finish: usize) -> (usize, Vec<usize>) {
        let mut gate = Fanout::default();
        let mut left: Vec<Option<usize>> = vec![None; n];
        let mut done = vec![false; n];
        let (mut peak, mut order) = (0, Vec::new());
        for _ in 0..1000 {
            let running = left.iter().flatten().count();
            let wanting: Vec<usize> = (0..n).filter(|&i| left[i].is_none() && !done[i]).collect();
            let mut turn = gate.begin(running, &wanting);
            for i in 0..n {
                if let Some(t) = &mut left[i] {
                    *t -= 1;
                    if *t == 0 { left[i] = None; done[i] = true; turn.finished(); }
                }
                if left[i].is_none() && !done[i] && turn.admit(i) {
                    left[i] = Some(pumps_to_finish);
                    order.push(i);
                }
            }
            peak = peak.max(left.iter().flatten().count());
            if done.iter().all(|&d| d) { break; }
        }
        assert!(done.iter().all(|&d| d), "a wanted server was never granted");
        (peak, order)
    }

    #[test]
    fn forty_servers_are_all_asked_and_never_more_than_four_at_once() {
        let (peak, order) = run(40, 3);
        assert_eq!(peak, WIDTH);
        let mut asked = order.clone();
        asked.sort_unstable();
        assert_eq!(asked, (0..40).collect::<Vec<_>>());
    }

    #[test]
    fn a_roster_within_the_width_is_granted_whole_in_the_stores_order() {
        let mut gate = Fanout::default();
        let mut turn = gate.begin(0, &[7, 9, 12, 20]);
        assert!([7, 9, 12, 20].iter().all(|&id| turn.admit(id)));
        // even when one of them was already out and another finishes: nothing is deferred
        let mut turn = gate.begin(2, &[3, 5]);
        assert!(turn.admit(3) && turn.admit(5));
    }

    #[test]
    fn grants_resume_after_where_the_last_pump_ended() {
        let mut gate = Fanout::default();
        let all: Vec<usize> = (0..10).collect();
        let mut turn = gate.begin(0, &all);
        let first: Vec<usize> = all.iter().copied().filter(|&i| turn.admit(i)).collect();
        assert_eq!(first, [0, 1, 2, 3]);
        // all four still out and a fifth wanting changes nothing; when they finish the next four
        // start after 3, not back at 0
        let mut turn = gate.begin(0, &all);
        let second: Vec<usize> = all.iter().copied().filter(|&i| turn.admit(i)).collect();
        assert_eq!(second, [4, 5, 6, 7]);
        let mut turn = gate.begin(0, &all);
        let third: Vec<usize> = all.iter().copied().filter(|&i| turn.admit(i)).collect();
        assert_eq!(third, [0, 1, 8, 9]);
    }

    #[test]
    fn room_left_by_a_finished_request_is_usable_in_the_same_pump() {
        let mut gate = Fanout::default();
        let mut turn = gate.begin(4, &[]);
        assert!(!turn.admit(1));
        turn.finished();
        assert!(turn.admit(1));
    }
}
