//! The one test that watches the Feed's diagnostics mirror, kept in media because its writers are
//! media's files too. The Feed's own mailbox and transport are `plx_play_feed::shared`.

#[cfg(test)]
mod tests {
    /// **Every `dg_*` mirror field must have a real writer.**
    ///
    /// The whole diagnostics mirror is write-here-read-there by construction, so a field that is
    /// declared, cleared in `reset_session`, and read by the panel compiles, runs, and is silently
    /// WRONG — there is no unused-field warning for it and no other test can see it. That is not
    /// hypothetical: `dg_place_rv`, `dg_placed_w`, `dg_placed_h` and `dg_splice` shipped exactly
    /// like that. `place_rv` stayed at its `i32::MIN` "never called" sentinel forever, so the
    /// read-out rendered `Placed: not placed` in DANGER bold on every webOS 5+ television — a
    /// fabricated fault, on the one firmware family we cannot test, aimed at the wrong layer.
    ///
    /// Source-level rather than behavioural because the writers live behind the ACB/Starfish seam,
    /// which does not exist on the host. `reset_session` is excluded deliberately: clearing a field
    /// is not writing it, and treating it as one is what let this through.
    #[test]
    fn every_diagnostics_mirror_field_has_a_writer() {
        const SHARED_SRC: &str = include_str!("../../../play-feed/src/shared.rs");
        // ff.rs is the THIRD writer of this block — the demux thread stamps the HTTP status and
        // the bytes it received. A writer file missing from this list reads exactly like a field
        // with no writer, so adding one here is part of adding one there.
        const WRITERS: [&str; 9] = [
            include_str!("pump.rs"),
            include_str!("../../../play-feed/src/feed_pump.rs"),
            include_str!("engine.rs"),
            include_str!("../../../play-feed/src/native_executor.rs"),
            include_str!("../../../play-feed/src/sf_event.rs"),
            include_str!("mod.rs"),
            include_str!("../ff.rs"),
            include_str!("../adaptive.rs"),
            include_str!("../../../play-feed/src/demux_clock.rs"),
        ];

        let declared: Vec<&str> = SHARED_SRC
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub dg_"))
            .filter_map(|l| l.split(':').next())
            .collect();
        assert!(
            declared.len() >= 10,
            "found only {} dg_ fields — the parse broke",
            declared.len()
        );
        // rustfmt may wrap `SHARED.dg_field.store(...)` between either dot.  Whitespace carries no
        // source-level meaning here, so remove it before looking for the writer expression.
        let writers: Vec<String> = WRITERS
            .iter()
            .map(|src| src.chars().filter(|c| !c.is_whitespace()).collect())
            .collect();
        // A first-wins latch is written by a compare-exchange inside a `Shared` method (the
        // demuxer calls the method), so that spelling counts, but only where it is declared: the
        // reset's plain `.store(0)` in this file must not satisfy the plain-store rule.
        let shared_src: String = SHARED_SRC.chars().filter(|c| !c.is_whitespace()).collect();
        for f in declared {
            let written = shared_src.contains(&format!("dg_{f}.compare_exchange("))
                || writers.iter().any(|src| {
                    src.contains(&format!("dg_{f}.store(")) || src.contains(&format!("dg_{f}.fetch_"))
                });
            assert!(written, "dg_{f} is declared and read but NOTHING writes it — the panel would print its sentinel as fact");
        }
    }

}
