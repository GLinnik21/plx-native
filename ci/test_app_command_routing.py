#!/usr/bin/env python3
"""Negative controls for compiler-backed, consuming external FIFO routing."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('app_command_routing', Path(__file__).with_name('check-app-command-routing.py'))
routing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(routing)

SOURCE = '''
enum AppFx { Loop(Req), Store(Id, Cmd) }
enum InlineAppFx { Store(Id, Cmd) }
enum ExternalAppFx { Loop(Req) }
impl AppEffect for AppFx {
    fn route(self) {
        match self {
            Self::Loop(v) => External(ExternalAppFx::Loop(v)),
            Self::Store(id, v) => Inline(InlineAppFx::Store(id, v)),
        }
    }
}
fn app_effect(fx: InlineAppFx) { match fx {
    InlineAppFx::Store(_, _) => out.push(Fx::Deliver(to, message)),
} }
fn execute_yielded_command() {
    let command = d.take_app_command()?;
    match command.effect {
        Ok(ExternalAppFx::Loop(request)) => execute_loop(request),
        Err(outcome) => outcome,
    }
}
'''


class RoutingTests(unittest.TestCase):
    def test_consuming_typed_partition_and_real_head_execution_pass(self):
        self.assertEqual(routing.violations(SOURCE), [])

    def test_external_operation_cannot_be_converted_to_inline(self):
        self.assertTrue(routing.violations(SOURCE.replace('External(ExternalAppFx::Loop(v))', 'Inline(ExternalAppFx::Loop(v))')))

    def test_external_variant_cannot_execute_inline(self):
        source = SOURCE.replace('match fx {', 'match fx { ExternalAppFx::Loop(request) => execute_loop(request),')
        self.assertTrue(routing.violations(source))

    def test_side_queue_cannot_hide_behind_unreachable(self):
        source = SOURCE.replace('match fx {', 'match fx { ExternalAppFx::Loop(request) => { self.reqs.push(request); unreachable!("yield") },')
        self.assertTrue(routing.violations(source))

    def test_missing_runner_arm_fails(self):
        self.assertTrue(routing.violations(SOURCE.replace('Ok(ExternalAppFx::Loop(request)) => execute_loop(request),', '')))

    def test_runner_cannot_take_a_different_queue(self):
        self.assertTrue(routing.violations(SOURCE.replace('d.take_app_command()?', 'self.take_reqs()?')))

    def test_comments_and_strings_cannot_supply_a_runner_arm(self):
        source = SOURCE.replace('Ok(ExternalAppFx::Loop(request)) => execute_loop(request),',
                                '// Ok(ExternalAppFx::Loop(request)) => execute_loop(request),\n')
        self.assertTrue(routing.violations(source))

    def test_boolean_classification_is_not_a_routing_proof(self):
        self.assertTrue(routing.violations(SOURCE + '\nfn external_app_fx() { helper(fx) }'))

    def test_second_head_take_in_runner_fails(self):
        source = SOURCE.replace('match command.effect', 'let extra = d.take_app_command()?; match command.effect')
        self.assertTrue(routing.violations(source))

    def test_head_take_in_another_drain_fails(self):
        self.assertTrue(routing.violations(SOURCE + '\nfn bypass() { d.take_app_command(); }'))

    def test_renamed_family_request_accumulator_fails(self):
        for name in ('LoopReq', 'ExternalAppFx', 'ResourceAppFx'):
            self.assertTrue(routing.violations(SOURCE + f'\nstruct Bridge {{ pending: Vec<(Source, {name})> }}'))

    def test_runner_arm_must_not_park_the_taken_request(self):
        self.assertTrue(routing.violations(SOURCE.replace('execute_loop(request)', 'self.pending.push(request)')))

    def test_taking_without_executing_cannot_claim_success(self):
        self.assertTrue(routing.violations(SOURCE.replace('execute_loop(request)', 'AppCommandOutcome::Completed')))

    def test_conversion_cannot_drop_or_duplicate_an_emission(self):
        arm = 'Self::Loop(v) => External(ExternalAppFx::Loop(v)),'
        self.assertTrue(routing.violations(SOURCE.replace(arm, '')))
        self.assertTrue(routing.violations(SOURCE.replace(arm, arm + arm)))

    def test_runner_catch_all_cannot_retain_an_external_command(self):
        source = SOURCE.replace('match command.effect {',
                                'match command.effect { other => { self.pending.push(other); AppCommandOutcome::Completed },')
        self.assertIn('runner must explicitly match external variants or Err', routing.violations(source))

    def test_runner_or_pattern_cannot_park_either_external_family(self):
        source = SOURCE.replace('enum ExternalAppFx { Loop(Req) }', 'enum ExternalAppFx { Loop(Req), Player(Req) }')
        source = source.replace('Ok(ExternalAppFx::Loop(request)) => execute_loop(request),',
                                'Ok(effect @ ExternalAppFx::Loop(_)) | Ok(effect @ ExternalAppFx::Player(_)) => { self.pending.push(effect); execute_loop(effect) },')
        self.assertIn('runner arm parks work instead of completing its taken head', routing.violations(source))

    def test_legitimate_async_execution_and_addressed_delivery_pass(self):
        source = SOURCE.replace('execute_loop(request)', 'execute_async_port(request)')
        self.assertEqual(routing.violations(source), [])

    def test_current_production_contract(self):
        self.assertEqual(routing.violations(
            Path('rust-modules/src/app/bridge.rs').read_text(encoding='utf-8'),
            Path('rust-modules/screens/src/registry.rs').read_text(encoding='utf-8')), [])


if __name__ == '__main__':
    unittest.main()
