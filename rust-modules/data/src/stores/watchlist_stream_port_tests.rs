use super::*;
use plx_machine::machine::{MachineId, RequestId};
use crate::stores::StoreId;

fn fixture<T: Send + 'static>() -> (StreamPort<T>, Addr, Tag, Arc<StreamMailbox<T>>) {
    let tag = Tag { profile_gen: 9, epoch: 4, req: 7 };
    let addr = Addr { to: MachineId::Store(StoreId::Watchlist.ord()), req: RequestId(tag.req) };
    let mailbox = Arc::new(StreamMailbox::default());
    (StreamPort::new(addr, tag, Arc::clone(&mailbox)), addr, tag, mailbox)
}

#[test]
fn stream_port_order_and_acknowledgement() {
    let (port, addr, tag, mailbox) = fixture();
    let producer = port.start().unwrap();
    assert!(producer.post(addr, 1));
    assert!(producer.post(addr, 2));
    producer.finish(StreamEnd::Done);
    assert!(port.is_owed());
    let batch = mailbox.take();
    assert_eq!(batch.len(), 3);
    assert!(batch.iter().all(|e| e.addr == addr && e.tag == tag));
    assert!(matches!(batch[0].event, StreamEvent::Post(1)));
    assert!(matches!(batch[1].event, StreamEvent::Post(2)));
    assert!(matches!(batch[2].event, StreamEvent::End(StreamEnd::Done)));
    assert!(!batch[0].ack_terminal());
    assert!(port.is_owed());
    assert!(batch[2].ack_terminal());
    assert!(!batch[2].ack_terminal());
    assert!(!port.is_owed());
}

#[test]
fn stream_port_terminal_before_start_and_refusal() {
    let (port, addr, _, mailbox) = fixture::<u8>();
    port.done(addr, StreamEnd::Done);
    assert!(port.start().is_none());
    port.refused(addr);
    let batch = mailbox.take();
    assert_eq!(batch.len(), 1);
    assert!(matches!(batch[0].event, StreamEvent::End(StreamEnd::Done)));
    assert!(port.is_owed());
    assert!(batch[0].ack_terminal());
    let (port, addr, _, mailbox) = fixture::<u8>();
    port.refused(addr);
    assert!(port.start().is_none());
    assert!(matches!(mailbox.take()[0].event, StreamEvent::End(StreamEnd::NotStarted)));
}

#[test]
fn stream_port_wrong_address_does_not_retire() {
    let (port, addr, _, mailbox) = fixture();
    let wrong = Addr { to: addr.to, req: RequestId(addr.req.0 + 1) };
    port.done(wrong, StreamEnd::Done);
    port.refused(wrong);
    assert!(!port.post(wrong, 1));
    let producer = port.start().unwrap();
    assert!(!producer.post(wrong, 2));
    assert!(producer.post(addr, 3));
    producer.finish(StreamEnd::Done);
    let batch = mailbox.take();
    assert_eq!(batch.len(), 2);
    assert!(matches!(batch[0].event, StreamEvent::Post(3)));
    assert!(matches!(batch[1].event, StreamEvent::End(StreamEnd::Done)));
}

#[test]
fn stream_port_duplicate_and_late_publications_are_ignored() {
    let (port, addr, _, mailbox) = fixture();
    assert!(!port.post(addr, 0));
    let producer = port.start().unwrap();
    assert!(producer.post(addr, 1));
    port.done(addr, StreamEnd::Done);
    assert!(!producer.post(addr, 2));
    port.done(addr, StreamEnd::Panicked);
    port.refused(addr);
    drop(producer);
    let batch = mailbox.take();
    assert_eq!(batch.len(), 2);
    assert!(matches!(batch[0].event, StreamEvent::Post(1)));
    assert!(matches!(batch[1].event, StreamEvent::End(StreamEnd::Done)));
}

#[test]
fn stream_port_pending_cancel_prevents_retained_start() {
    let (port, addr, _, mailbox) = fixture::<u8>();
    let retained = port.clone();
    port.cancel();
    port.cancel();
    port.refused(addr);
    assert!(retained.start().is_none());
    assert!(retained.is_cancelled());
    let batch = mailbox.take();
    assert_eq!(batch.len(), 1);
    assert!(matches!(batch[0].event, StreamEvent::End(StreamEnd::Cancelled)));
    assert!(retained.is_owed());
    assert!(batch[0].ack_terminal());
}

#[test]
fn stream_port_started_cancel_is_a_latch_not_early_terminal() {
    let (port, addr, _, mailbox) = fixture();
    let producer = port.start().unwrap();
    port.cancel();
    port.refused(addr);
    port.done(addr, StreamEnd::NotStarted);
    assert!(producer.is_cancelled());
    assert!(mailbox.take().is_empty());
    assert!(port.is_owed());
    assert!(producer.post(addr, 7));
    producer.finish(StreamEnd::Cancelled);
    let batch = mailbox.take();
    assert_eq!(batch.len(), 2);
    assert!(matches!(batch[0].event, StreamEvent::Post(7)));
    assert!(matches!(batch[1].event, StreamEvent::End(StreamEnd::Cancelled)));
}

#[test]
fn stream_port_panic_and_producer_drop_complete_once() {
    let (port, addr, _, mailbox) = fixture::<u8>();
    let producer = port.start().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _producer = producer;
        panic!("scripted worker panic");
    }));
    assert!(result.is_err());
    port.done(addr, StreamEnd::Done);
    let batch = mailbox.take();
    assert_eq!(batch.len(), 1);
    assert!(matches!(batch[0].event, StreamEvent::End(StreamEnd::Panicked)));
    let (port, _, _, mailbox) = fixture::<u8>();
    drop(port.start().unwrap());
    assert!(matches!(mailbox.take()[0].event, StreamEvent::End(StreamEnd::Cancelled)));
}

#[test]
fn stream_port_drain_moves_nonclone_payload_and_keeps_later_batch() {
    struct Payload(Vec<u8>);
    let (port, addr, _, mailbox) = fixture();
    let producer = port.start().unwrap();
    let bytes = vec![4, 5, 6];
    let original = bytes.as_ptr();
    assert!(producer.post(addr, Payload(bytes)));
    let first = mailbox.take();
    assert!(!mailbox.has_mail());
    assert!(producer.post(addr, Payload(vec![7])));
    producer.finish(StreamEnd::Done);
    let second = mailbox.take();
    match &first[0].event {
        StreamEvent::Post(payload) => assert_eq!(payload.0.as_ptr(), original),
        _ => panic!("expected payload"),
    }
    assert_eq!(second.len(), 2);
    assert!(matches!(second[1].event, StreamEvent::End(StreamEnd::Done)));
}

#[test]
fn stream_port_terminal_never_overtakes_concurrent_posts() {
    for _ in 0..100 {
        let (port, addr, _, mailbox) = fixture();
        let producer = port.start().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            for i in 0..10 { producer.post(addr, i); }
            producer.finish(StreamEnd::Done);
        });
        barrier.wait();
        port.done(addr, StreamEnd::Done);
        worker.join().unwrap();
        let batch = mailbox.take();
        assert!(matches!(batch.last().unwrap().event, StreamEvent::End(StreamEnd::Done)));
        assert_eq!(batch.iter().filter(|landing| matches!(landing.event, StreamEvent::End(_))).count(), 1);
    }
}

#[test]
fn stream_port_start_and_cancel_compete_across_threads() {
    for _ in 0..100 {
        let (port, _, _, mailbox) = fixture::<u8>();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = port.clone();
        let sync = Arc::clone(&barrier);
        let worker = std::thread::spawn(move || {
            sync.wait();
            if let Some(producer) = other.start() {
                if producer.is_cancelled() { producer.finish(StreamEnd::Cancelled); }
                else { producer.finish(StreamEnd::Done); }
            }
        });
        barrier.wait();
        port.cancel();
        worker.join().unwrap();
        let batch = mailbox.take();
        assert_eq!(batch.len(), 1);
        assert!(matches!(batch[0].event, StreamEvent::End(StreamEnd::Done | StreamEnd::Cancelled)));
        assert!(batch[0].ack_terminal());
    }
}
