#!/usr/bin/env python3
"""Pins actual Sans-IO runtime boundary sites; behavior is exercised by Rust negative tests."""
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
class StepBoundaries(unittest.TestCase):
    def source(self, path): return (ROOT / 'rust-modules' / path).read_text(encoding='utf-8')
    def test_screen_scope_covers_only_the_step_not_drain_or_instruments(self):
        source = self.source('ui/src/dispatch.rs')
        start = source.index('let handled = plx_base::diag::spans::span("dstep"')
        end = source.index('drop(fx);', start)
        window = source[start:end]
        self.assertEqual(window.count('StepScope::enter()'), 1)
        self.assertLess(window.index('StepScope::enter()'), window.index('inst.screen.step('))
        self.assertTrue(window.rstrip().endswith('});'))
        self.assertEqual(source.count('StepScope::enter()'), 1)
    def test_all_actual_io_funnels_have_step_assertions(self):
        for path, operations in [
            ('base/src/task.rs', ('task::try_spawn', 'task::beside_with')),
            ('base/src/storage_worker.rs', ('storage_worker::submit', 'storage_worker::submit_retained',
                'storage_worker::pump_retained', 'storage_worker::Executor::submit', 'storage_worker::Writer::submit')),
            ('base/src/clock.rs', ('clock::vclock_ms', 'clock::vclock_us', 'clock::wall_now')),
            ('src/app/clock.rs', ('app::clock::now', 'app::clock::unix_ms', 'app::clock::today'))]:
            source = self.source(path)
            for operation in operations:
                self.assertIn('assert_io_allowed("' + operation + '")', source)
        source = self.source('base/src/task/blocking.rs')
        body = source[source.index('pub fn assert_may_block('):]
        self.assertLess(body.index('assert_io_allowed('), body.index('ALLOWED.with('))
    def test_store_guards_never_wrap_job_drain(self):
        source = self.source('data/src/stores/mod.rs')
        start = source.index('pub fn take_jobs(')
        end = source.index('pub fn metadata_pump(', start)
        self.assertNotIn('StepScope', source[start:end])
        self.assertGreaterEqual(source.count('StepScope::enter()'), 20)
    def test_scope_has_no_release_tls_and_cannot_cross_threads(self):
        source = self.source('base/src/step.rs')
        self.assertIn('#[cfg(any(test, feature = "test-support"))]\nthread_local!', source)
        self.assertIn('PhantomData<Rc<()>>', source)
        self.assertIn('impl Drop for StepScope', source)
if __name__ == '__main__': unittest.main()
