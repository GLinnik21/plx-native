//! Best-effort animation diagnostic runner. Only the app frame tail admits work;
//! the shared bounded storage executor serializes accepted batches off-frame.
//! Overflow drops newest samples, admission refusal drops the batch, and a write
//! error stops that batch without retry. Diagnostics never promise durable storage.

use plx_base::storage_worker::SubmitError;

type Batch = (Vec<String>, usize);

pub(super) fn drain() -> Result<usize, SubmitError> {
    drain_with(plx_ui::anim::take_settle_log(), |batch| {
        // Capture the instance root on the app boundary, not in the machine or worker.
        let path = plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::ANIMATION);
        plx_base::storage_worker::submit(move || {
            write_batch(batch, |line| plx_base::eventlog::append_record(&path, line))
        })
    })
}

fn drain_with<R>(batch: Batch, submit: impl FnOnce(Batch) -> Result<R, SubmitError>)
    -> Result<usize, SubmitError>
{
    plx_base::step::assert_io_allowed("animation diagnostic runner");
    let count = batch.0.len();
    if count == 0 && batch.1 == 0 { return Ok(0); }
    // Dropping a ticket does not cancel accepted work. Results are intentionally
    // best-effort, just like the previous sink's ignored open/write errors.
    submit(batch).map(|_| count)
}

fn write_batch(batch: Batch, mut write: impl FnMut(&str) -> std::io::Result<()>)
    -> std::io::Result<usize>
{
    plx_base::step::assert_io_allowed("animation diagnostic sink");
    let count = batch.0.len();
    for record in batch.0 { write(&record)?; }
    if batch.1 != 0 { write(&format!("anim: dropped {} settle records (buffer full)", batch.1))?; }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_runner_is_outside_step_and_does_not_retry_refused_work() {
        let step = plx_base::step::StepScope::enter();
        assert!(std::panic::catch_unwind(|| drain_with((vec!["one".into()], 0), |_| Ok(()))).is_err());
        assert!(std::panic::catch_unwind(|| write_batch((vec![], 0), |_| Ok(()))).is_err());
        drop(step);
        let mut calls = 0;
        assert_eq!(drain_with((vec!["one".into()], 0), |_| { calls += 1; Err::<(), _>(SubmitError::Full) }), Err(SubmitError::Full));
        assert_eq!(calls, 1);
        assert_eq!(drain_with((vec![], 0), |_| -> Result<(), SubmitError> { panic!("empty batch must not start IO") }), Ok(0));
    }

    #[test]
    fn diagnostic_batch_keeps_order_reports_overflow_and_stops_after_write_failure() {
        let mut lines = Vec::new();
        let batch = (vec!["first".into(), "second".into()], 3);
        assert_eq!(write_batch(batch, |s| { lines.push(s.to_owned()); Ok(()) }).unwrap(), 2);
        assert_eq!(lines, ["first", "second", "anim: dropped 3 settle records (buffer full)"]);
        let mut calls = 0;
        let result = write_batch((vec!["first".into(), "second".into()], 0), |_| {
            calls += 1;
            Err(std::io::Error::other("refused sink"))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
