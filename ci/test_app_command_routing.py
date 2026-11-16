#!/usr/bin/env python3
"""Negative controls for the external FIFO routing proof."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('app_command_routing', Path(__file__).with_name('check-app-command-routing.py'))
routing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(routing)

SOURCE = '''
fn app_effect() { match fx {
    AppFx::Loop(_) => unreachable!("yield"),
    AppFx::Store(_, _) => out.push(Fx::Deliver(to, message)),
} }
fn external_app_fx() { matches!(fx, AppFx::Loop(_)) }
fn execute_yielded_command() {
    let command = d.take_app_command()?;
    match command.effect {
        Ok(AppFx::Loop(request)) => execute_loop(request),
        Ok(_) => unreachable!("not external"),
    }
}
'''


class RoutingTests(unittest.TestCase):
    def test_explicit_yield_and_real_head_execution_pass(self):
        self.assertEqual(routing.violations(SOURCE), [])

    def test_unreachable_without_external_classification_fails(self):
        self.assertTrue(routing.violations(SOURCE.replace('matches!(fx, AppFx::Loop(_))', 'false')))

    def test_external_variant_with_side_queue_fails(self):
        self.assertTrue(routing.violations(SOURCE.replace('unreachable!("yield")', 'self.reqs.push(request)')))

    def test_side_queue_cannot_hide_behind_unreachable(self):
        self.assertTrue(routing.violations(SOURCE.replace('unreachable!("yield")', '{ self.reqs.push(request); unreachable!("yield") }')))

    def test_missing_runner_arm_fails(self):
        self.assertTrue(routing.violations(SOURCE.replace('Ok(AppFx::Loop(request)) => execute_loop(request),', '')))

    def test_runner_cannot_take_a_different_queue(self):
        self.assertTrue(routing.violations(SOURCE.replace('d.take_app_command()?', 'self.take_reqs()?')))

    def test_comments_and_strings_cannot_supply_a_runner_arm(self):
        source = SOURCE.replace('Ok(AppFx::Loop(request)) => execute_loop(request),',
                                '// Ok(AppFx::Loop(request)) => execute_loop(request),\n')
        self.assertTrue(routing.violations(source))

    def test_delegated_classification_is_not_a_routing_proof(self):
        self.assertTrue(routing.violations(SOURCE.replace('matches!(fx, AppFx::Loop(_))', 'helper(fx)')))

    def test_second_head_take_in_runner_fails(self):
        source = SOURCE.replace('match command.effect', 'let extra = d.take_app_command()?; match command.effect')
        self.assertTrue(routing.violations(source))

    def test_head_take_in_another_drain_fails(self):
        self.assertTrue(routing.violations(SOURCE + '\nfn bypass() { d.take_app_command(); }'))

    def test_renamed_family_request_accumulator_fails(self):
        self.assertTrue(routing.violations(SOURCE + '\nstruct Bridge { pending: Vec<(Source, LoopReq)> }'))

    def test_runner_arm_must_not_park_the_taken_request(self):
        source = SOURCE.replace('execute_loop(request)', 'self.pending.push(request)')
        self.assertTrue(routing.violations(source))


if __name__ == '__main__':
    unittest.main()
