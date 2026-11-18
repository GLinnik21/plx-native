#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Paused,
    Waiting,
    Playing,
}

impl Phase {
    pub fn parse(name: &str) -> Option<Phase> {
        match name {
            "idle" => Some(Phase::Idle),
            "paused" => Some(Phase::Paused),
            "waiting" => Some(Phase::Waiting),
            "playing" => Some(Phase::Playing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timeline {
    pub phase: Phase,
    pub position_ms: i64,
    pub at_ms: i64,
    pub rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub position_ms: i64,
    pub running: bool,
}

impl Timeline {
    pub fn at(&self, duration_ms: i64, relay_now_ms: i64) -> Position {
        match self.phase {
            Phase::Playing => {
                let elapsed = (relay_now_ms - self.at_ms).max(0) as f64;
                let advanced = elapsed * self.rate;
                let sum = self.position_ms as f64 + advanced;
                Position { position_ms: (sum.floor() as i64).min(duration_ms), running: relay_now_ms >= self.at_ms }
            }
            Phase::Idle | Phase::Paused | Phase::Waiting => Position { position_ms: self.position_ms, running: false },
        }
    }
}
