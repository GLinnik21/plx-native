//! Stream transport tests; synthetic payloads, locally owned queues, no adapters or network.
#[cfg(test)]
mod tests {
    use super::super::*;
    use std::sync::Arc;

    fn addr(req: u32) -> Addr { Addr { to: MachineId::Session, req: RequestId(req) } }
    fn drain(l: &Landing<u32, u32>) -> Vec<Landed<u32, u32>> {
        let mut out = Vec::new();
        l.take_for(&|_| true, &|_| true, &mut out);
        out
    }

    #[test]
    fn progress_drains_keep_one_reservation_and_fifo() {
        let l = Landing::with_limits(2, 1, 1);
        l.admit_stream(addr(1)).unwrap();
        let mut last = 0;
        for n in 0..3 {
            l.progress(addr(1), 0, n).unwrap();
            let out = drain(&l);
            assert_eq!(out.len(), 1);
            assert!(!out[0].terminal);
            assert!(matches!(out[0].lane, Lane::Data(0, value) if value == n));
            assert!(out[0].seq > last); last = out[0].seq;
            assert_eq!(l.inflight(MachineId::Session), 1);
            assert_eq!(l.admit(addr(2)), Err(AdmissionError::Capacity));
        }
        l.put(addr(1), 0, 9).unwrap();
        assert!(drain(&l)[0].terminal);
        assert_eq!(l.inflight(MachineId::Session), 0);
    }

    #[test]
    fn stream_terminal_bypasses_full_data_and_interleaves_with_other_streams() {
        let l = Landing::with_limits(2, 2, 2);
        l.admit_stream(addr(1)).unwrap(); l.admit_stream(addr(2)).unwrap();
        l.progress(addr(1), 0, 10).unwrap(); l.progress(addr(2), 0, 20).unwrap();
        l.put(addr(1), 0, 11).unwrap();
        let out = drain(&l);
        assert_eq!(out.iter().map(|r| (r.addr.req.0, r.terminal)).collect::<Vec<_>>(),
            vec![(1, false), (2, false), (1, true)]);
        assert!(matches!(out[2].lane, Lane::Data(0, 11)));
        assert!(out.windows(2).all(|r| r[0].seq < r[1].seq));
        assert_eq!(l.inflight(MachineId::Session), 1, "only A retired");
        l.progress(addr(2), 0, 21).unwrap(); l.put(addr(2), 0, 22).unwrap();
        assert_eq!(drain(&l).len(), 2);
    }

    #[test]
    fn overflow_closes_stream_once_and_cannot_become_success() {
        let l = Landing::new(1);
        l.admit_stream(addr(1)).unwrap();
        l.progress(addr(1), 0, 10).unwrap();
        assert_eq!(l.progress(addr(1), 0, 11), Err(PublishError::Full));
        assert_eq!(l.progress(addr(1), 0, 12), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.put(addr(1), 0, 13), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.dropped(addr(1)), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.refused(addr(1)), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.inflight(MachineId::Session), 1);
        let out = drain(&l);
        assert_eq!(out.len(), 2);
        assert!(!out[0].terminal && out[1].terminal);
        assert!(out[0].seq < out[1].seq);
        assert!(matches!(out[1].lane, Lane::Dropped(RequestId(1))));
        assert_eq!(l.dropped_for(MachineId::Session), 1);
    }

    #[test]
    fn progress_rejects_unknown_one_shot_cancelled_and_closed_without_accounting() {
        let l = Landing::new(2);
        l.admit(addr(1)).unwrap(); l.admit_stream(addr(2)).unwrap();
        assert_eq!(l.progress(addr(9), 0, 0), Err(PublishError::Unknown));
        assert_eq!(l.progress(addr(1), 0, 0), Err(PublishError::NotStream));
        l.cancel(addr(2));
        assert_eq!(l.progress(addr(2), 0, 0), Err(PublishError::Cancelled));
        l.put(addr(1), 0, 1).unwrap();
        assert_eq!(l.progress(addr(1), 0, 0), Err(PublishError::AlreadyTerminal));
        assert_eq!(l.inflight(MachineId::Session), 2);
        assert_eq!(l.len(), 1);
        assert_eq!(l.dropped_count(), 0);
        assert!(drain(&l)[0].terminal, "one-shot consumers only receive terminals");
        l.dropped(addr(2)).unwrap(); drain(&l);
    }

    #[test]
    fn exact_cancel_discards_only_a_and_sequences_its_ack() {
        for ack_first in [false, true] {
            let l = Landing::with_limits(4, 2, 2);
            l.admit_stream(addr(1)).unwrap(); l.admit_stream(addr(2)).unwrap();
            l.progress(addr(1), 0, 1).unwrap(); l.progress(addr(2), 0, 2).unwrap();
            assert!(l.cancel(addr(1)));
            assert_eq!(l.dropped_count(), 1, "queued A progress discarded on main");
            if ack_first { l.put(addr(1), 0, 3).unwrap(); }
            l.put(addr(2), 0, 4).unwrap();
            if !ack_first { l.put(addr(1), 0, 3).unwrap(); }
            let seqs: Vec<_> = l.lock().control.iter().map(|r| (r.addr.req.0, r.seq)).collect();
            assert_eq!(seqs[0].0, if ack_first { 1 } else { 2 });
            assert!(seqs[0].1 < seqs[1].1);
            assert_eq!(l.dropped_count(), 1, "worker acknowledgement not counted yet");
            assert_eq!(l.admit_stream(addr(3)), Err(AdmissionError::Capacity));
            let out = drain(&l);
            assert_eq!(out.len(), 2);
            assert!(out.iter().all(|r| r.addr == addr(2)));
            assert_eq!(l.dropped_count(), 2);
            assert_eq!(l.put(addr(1), 0, 3), Err(PublishError::Unknown));
            assert!(!l.cancel(addr(9)));
        }
    }

    #[test]
    fn clear_twice_keeps_running_slots_and_discards_queued_terminals_once() {
        let l = Landing::new(4);
        l.admit_stream(addr(1)).unwrap(); l.admit_stream(addr(2)).unwrap();
        l.progress(addr(1), 0, 1).unwrap(); l.put(addr(1), 0, 2).unwrap();
        l.progress(addr(2), 0, 3).unwrap();
        l.clear(); l.clear();
        assert_eq!(l.dropped_count(), 3);
        assert_eq!(l.inflight(MachineId::Session), 1);
        l.dropped(addr(2)).unwrap();
        assert_eq!(l.inflight(MachineId::Session), 1);
        assert_eq!(l.dropped_count(), 3);
        assert!(drain(&l).is_empty());
        assert_eq!(l.dropped_count(), 4);
    }

    #[test]
    fn guarded_workers_finish_abandon_or_unwind_exactly_once() {
        for (finish, panic_worker, cancel) in [(false, false, false), (true, false, false),
            (false, true, false), (true, true, false), (false, true, true)] {
            let l = Arc::new(Landing::new(1));
            l.admit_stream(addr(1)).unwrap();
            if cancel { l.cancel(addr(1)); }
            let worker = Arc::clone(&l);
            let joined = std::thread::spawn(move || {
                let _completion = worker.completion_guard(addr(1)).unwrap();
                if !cancel { worker.progress(addr(1), 0, 1).unwrap(); }
                if finish { worker.put(addr(1), 0, 2).unwrap(); }
                if !finish && !panic_worker { return; }
                if panic_worker { panic!("synthetic worker unwind"); }
                // Early return without finish also runs the completion guard.
            }).join();
            assert_eq!(joined.is_err(), panic_worker);
            assert_eq!(l.inflight(MachineId::Session), 1);
            let out = drain(&l);
            assert_eq!(out.iter().filter(|r| r.terminal).count(), usize::from(!cancel));
            if !cancel {
                let terminal = &out.last().unwrap().lane;
                assert!(if finish { matches!(terminal, Lane::Data(0, 2)) }
                    else { matches!(terminal, Lane::Dropped(RequestId(1))) });
            }
            assert_eq!(l.inflight(MachineId::Session), 0);
        }
        let l: Landing<u32, u32> = Landing::new(1);
        l.admit_stream(addr(1)).unwrap();
        // Simulated OS spawn refusal: closure never starts, so no guard is constructed.
        l.refused(addr(1)).unwrap();
        assert!(matches!(drain(&l)[0].lane, Lane::Refused(RequestId(1))));
        assert_eq!(l.inflight(MachineId::Session), 0);
    }

    #[test]
    fn stream_terminals_and_rejected_attempts_obey_both_admission_caps() {
        let l = Landing::with_limits(1, 1, 2);
        l.admit_stream(addr(1)).unwrap();
        assert_eq!(l.admit(addr(1)), Err(AdmissionError::Duplicate));
        assert_eq!(l.admit_stream(addr(2)), Err(AdmissionError::Capacity));
        let cache = Addr { to: MachineId::Cache, req: RequestId(1) };
        l.admit_stream(cache).unwrap();
        l.progress(addr(1), 0, 1).unwrap();
        l.put(addr(1), 0, 2).unwrap(); l.put(cache, 0, 3).unwrap();
        for n in 2..1002 {
            assert_eq!(l.admit_stream(Addr { to: MachineId::Player, req: RequestId(n) }), Err(AdmissionError::Capacity));
            assert_eq!(l.admit_stream(addr(2)), Err(AdmissionError::Capacity));
        }
        assert_eq!(l.len(), 3, "one capped progress record plus two reserved terminals");
        assert_eq!(drain(&l).len(), 3);
    }
}
