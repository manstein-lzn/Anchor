"""Behavioral concurrency and recovery checks for one Run with paired control nodes."""
from __future__ import annotations

import json
import threading
from types import SimpleNamespace

import pytest

from anchor.node import node_key
from anchor.node.recovery import CompletionFact, record_completion
from anchor.simple import run as runner


def definition(chain=False):
    nodes = [{'id': 'split', 'op': 'split'}, {'id': 'left', 'agent': 'worker'},
             {'id': 'right', 'agent': 'worker'}, {'id': 'collect', 'op': 'collect'},
             {'id': 'after', 'agent': 'worker'}]
    edges = [('split', 'left'), ('split', 'right'), ('left', 'collect'),
             ('right', 'collect'), ('collect', 'after')]
    if chain:
        nodes.append({'id': 'left2', 'agent': 'worker'})
        edges.remove(('left', 'collect'))
        edges.extend([('left', 'left2'), ('left2', 'collect')])
    return {'entry': 'split', 'agents': {'worker': {'model': 'test'}},
            'ops': {'split': {'fanout': {'join': 'collect'}}, 'collect': {'join': {}}},
            'nodes': nodes, 'edges': [{'from': a, 'to': b} for a, b in edges]}


def setup(tmp_path, monkeypatch, behavior, raw=None):
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    (workspace / 'graph.json').write_text(json.dumps(raw or definition()))
    monkeypatch.setattr(runner, '_config', lambda _: ({}, None))
    executions = []

    def factory(graph, node_id, directory, *args, **options):
        class Worker:
            env = SimpleNamespace(route=None)

            def run(self, task=None, resume_mark=False):
                executions.append((node_id, resume_mark))
                return behavior(node_id, directory, options, self.env, resume_mark)

            def resume(self):
                return self.run(resume_mark=True)
        return Worker()
    monkeypatch.setattr(runner, '_agent_for', factory)
    return workspace, executions


def success(directory, text='done'):
    (directory / 'result.txt').write_text(text)
    return {'exit_status': 'Submitted', 'submission': text}


def execute(workspace, **kwargs):
    return runner.run(workspace, config_path='unused', run_id='one', **kwargs)


def test_branches_overlap_in_one_run_and_commit_on_coordinator(tmp_path, monkeypatch):
    barrier = threading.Barrier(2)
    coordinator = threading.get_ident()
    frozen = []
    freeze = runner._freeze

    def recording_freeze(*args):
        frozen.append(threading.get_ident())
        return freeze(*args)
    monkeypatch.setattr(runner, '_freeze', recording_freeze)

    def work(node, directory, options, env, resumed):
        if node in ('left', 'right'):
            assert threading.get_ident() != coordinator
            barrier.wait(timeout=10)  # Serial execution fails here, regardless of elapsed timings.
        if node == 'after':
            inputs = {item.node_id: item for item in options['inputs']}
            manifest = json.loads((inputs['collect'].tree / 'join.json').read_text())
            assert manifest['invocation'] == 1
            assert [item['output'] for item in manifest['branches']] == ['left', 'right']
            for branch in manifest['branches']:
                result = branch['nodes'][0]
                assert inputs[result['node']].commit == result['commit']
                assert (inputs[result['node']].tree / 'result.txt').read_text() == result['node']
        return success(directory, node)

    workspace, executions = setup(tmp_path, monkeypatch, work)
    state = execute(workspace)
    assert state.status == 'finished'
    assert set(frozen) == {coordinator}
    assert len(list((workspace / 'runs').iterdir())) == 1
    assert state.cursor is None and state.active == {} and state.parallel is None
    assert state.executed[0] == 'split' and state.executed[-2:] == ['collect', 'after']
    assert len(executions) == 3


def test_linear_branch_can_progress_while_other_branch_is_running(tmp_path, monkeypatch):
    left2_started = threading.Event()

    def work(node, directory, options, env, resumed):
        if node == 'right':
            assert left2_started.wait(10)
        if node == 'left2':
            assert next(item for item in options['inputs'] if item.node_id == 'left').direct
            left2_started.set()
        return success(directory, node)
    workspace, _ = setup(tmp_path, monkeypatch, work, definition(chain=True))
    assert execute(workspace).status == 'finished'


def test_failure_cancels_sibling_and_never_releases_join(tmp_path, monkeypatch):
    started, cancelled = threading.Event(), threading.Event()

    def work(node, directory, options, env, resumed):
        if node == 'left':
            assert started.wait(10)
            return {'exit_status': 'Failed', 'submission': 'bad evidence'}
        assert node == 'right'
        started.set()
        for _ in range(1000):
            if options['cancelled']():
                cancelled.set()
                return {'exit_status': 'Stopped'}
            threading.Event().wait(0.01)
        pytest.fail('sibling was not canceled')

    workspace, executions = setup(tmp_path, monkeypatch, work)
    state = execute(workspace)
    assert state.status == 'failed' and cancelled.is_set()
    assert 'collect' not in state.executed and 'right' in state.active
    assert not state.nodes['left']['submitted']
    resumed = execute(workspace, resume=workspace / 'runs/one')
    assert resumed.status == 'failed' and len(executions) == 2


def test_stop_retains_unfinished_identity_and_resume_keeps_completed_work(tmp_path, monkeypatch):
    started = threading.Event()
    request = {'value': None}

    def work(node, directory, options, env, resumed):
        if node == 'left':
            assert started.wait(10)
            request['value'] = 'stopped'
        if node == 'right' and not resumed:
            started.set()
            for _ in range(1000):
                if options['cancelled']():
                    return {'exit_status': 'Stopped'}
                threading.Event().wait(0.01)
            pytest.fail('stop was not delivered')
        return success(directory, node)

    workspace, executions = setup(tmp_path, monkeypatch, work)
    state = execute(workspace, stop_request=lambda: request['value'])
    assert state.status == 'stopped' and 'collect' not in state.executed
    assert state.active['right']['run'] == 1
    request['value'] = None
    state = execute(workspace, resume=workspace / 'runs/one')
    assert state.status == 'finished' and state.runs['right'] == 1
    assert executions.count(('left', False)) == 1
    assert ('right', True) in executions


def test_pause_drains_active_nodes_without_starting_next_branch_step(tmp_path, monkeypatch):
    started = threading.Event()
    request = {'value': None}

    def work(node, directory, options, env, resumed):
        if node == 'left':
            assert started.wait(10)
            request['value'] = 'paused'
        if node == 'right':
            started.set()
            assert not options['cancelled']()
        return success(directory, node)

    workspace, executions = setup(tmp_path, monkeypatch, work, definition(chain=True))
    state = execute(workspace, stop_request=lambda: request['value'])
    assert state.status == 'paused' and state.active == {}
    assert 'left2' not in state.executed and 'collect' not in state.executed
    request['value'] = None
    state = execute(workspace, resume=workspace / 'runs/one')
    assert state.status == 'finished'
    assert executions.count(('left', False)) == executions.count(('right', False)) == 1


def test_outer_feedback_waits_for_fresh_results_from_every_branch(tmp_path, monkeypatch):
    raw = definition()
    raw['nodes'].append({'id': 'done', 'agent': 'worker'})
    raw['edges'].extend([{'from': 'after', 'to': 'split'}, {'from': 'after', 'to': 'done'}])
    barrier = threading.Barrier(2)
    counts = {'left': 0, 'right': 0, 'after': 0}
    manifests = []

    def work(node, directory, options, env, resumed):
        if node in ('left', 'right'):
            counts[node] += 1
            barrier.wait(10)
        if node == 'after':
            counts[node] += 1
            manifest = json.loads((next(x for x in options['inputs'] if x.node_id == 'collect').tree / 'join.json').read_text())
            manifests.append(manifest)
            assert manifest['invocation'] == counts[node]
            assert counts['left'] == counts['right'] == counts[node]
            env.route = 'split' if counts[node] == 1 else 'done'
        return success(directory, f'{node}:{counts.get(node, 0)}')

    workspace, _ = setup(tmp_path, monkeypatch, work, raw)
    state = execute(workspace)
    assert state.status == 'finished' and state.runs['collect'] == 2
    assert manifests[0]['branches'][0]['nodes'][0]['commit'] != manifests[1]['branches'][0]['nodes'][0]['commit']


def test_completed_native_fact_survives_coordinator_failure_without_reexecution(tmp_path, monkeypatch):
    barrier = threading.Barrier(2)

    def work(node, directory, options, env, resumed):
        result = success(directory, node)
        if node in ('left', 'right'):
            record_completion(options['control'], CompletionFact(
                node=node_key(node), run='native-' + node, kind='agent', submission=node,
                route=None, command='', at='2026-10-01T00:00:00Z'))
            barrier.wait(10)
        return result

    workspace, executions = setup(tmp_path, monkeypatch, work)
    original_record = runner._record
    crashed = {'value': False}

    def record(*args):
        if args[4].node_id == 'left' and not crashed['value']:
            crashed['value'] = True
            raise RuntimeError('injected before graph records native completion')
        return original_record(*args)
    monkeypatch.setattr(runner, '_record', record)
    with pytest.raises(RuntimeError, match='injected'):
        execute(workspace)
    state = runner.RunState.load(workspace / 'runs/one')
    assert state.status == 'interrupted' and 'left' in state.active
    assert execute(workspace, resume=workspace / 'runs/one').status == 'finished'
    assert executions.count(('left', False)) == executions.count(('right', False)) == 1
    assert not any(resumed for _, resumed in executions)


def test_provider_free_real_nodes_sandbox_and_native_persistence(tmp_path):
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    (workspace / 'graph.json').write_text(json.dumps(definition()))
    config = tmp_path / 'config.json'
    config.write_text('{}')
    scripts = {'left': ["echo left > result.txt"], 'right': ["echo right > result.txt"],
               'after': ["cat /in/left/result.txt /in/right/result.txt > combined.txt"]}
    state = runner.run(workspace, config_path=config, model_script=scripts, run_id='native')
    assert state.status == 'finished'
    run_dir = workspace / 'runs/native'
    assert (run_dir / 'after/combined.txt').read_text() == 'left\nright\n'
    for node in ('left', 'right', 'after'):
        assert runner._completion_of(runner._control_path(
            runner.graph_module.parse(definition()), run_dir, node, 1), node) is not None


def test_legacy_run_state_defaults_do_not_require_migration(tmp_path):
    (tmp_path / 'run.json').write_text(json.dumps({'objective': 'old', 'started': 'old'}))
    state = runner.RunState.load(tmp_path)
    assert state.active == {} and state.parallel is None


def test_canceled_conversation_branches_preserve_artifacts_for_next_turn_and_resume(tmp_path, monkeypatch):
    barrier = threading.Barrier(2)
    request = {'value': None}
    followup = {'value': False}

    def work(node, directory, options, env, resumed):
        if node in ('left', 'right') and not followup['value']:
            (directory / 'partial.txt').write_text(node + ' unfinished')
            barrier.wait(10)
            request['value'] = 'stopped'
            return {'exit_status': 'Stopped'}
        if node in ('left', 'right') and followup['value'] and not resumed:
            previous = next(source for source, target in options['resources'] if target == '/previous')
            from pathlib import Path
            assert (Path(previous) / 'partial.txt').read_text() == node + ' unfinished'
            assert options['previous_steps']
        return success(directory, node)

    workspace, _ = setup(tmp_path, monkeypatch, work)
    state = execute(workspace, conversation_id='conv', stop_request=lambda: request['value'])
    assert state.status == 'stopped' and set(state.active) == {'left', 'right'}
    assert not state.nodes['left']['submitted'] and state.nodes['left']['commit']
    followup['value'] = True
    request['value'] = None
    following = runner.run(workspace, config_path='unused', run_id='next',
                           conversation_id='conv', previous_runs=(workspace / 'runs/one',))
    assert following.status == 'finished'
    resumed = execute(workspace, resume=workspace / 'runs/one', conversation_id='conv')
    assert resumed.status == 'finished' and resumed.runs['left'] == 1


def test_branch_ceiling_stays_stopped_across_resume(tmp_path, monkeypatch):
    raw = definition()
    raw['nodes'][1]['max_rounds'] = 1
    raw['edges'].append({'from': 'after', 'to': 'split'})
    workspace, _ = setup(tmp_path, monkeypatch,
                         lambda node, directory, *_: success(directory, node), raw)
    state = execute(workspace)
    assert (state.status, state.reason) == ('stopped', 'max_rounds')
    state = execute(workspace, resume=workspace / 'runs/one')
    assert (state.status, state.reason) == ('stopped', 'max_rounds')
    assert state.runs['collect'] == 1


def test_parallel_control_and_trace_paths_cannot_alias(tmp_path):
    # Paths encode identities independently of topology; use an extra legal serial node.
    raw = definition()
    raw['nodes'].append({'id': 'left-2', 'agent': 'worker'})
    graph = runner.graph_module.parse(raw)
    assert runner._control_path(graph, tmp_path, 'left', 2) != runner._control_path(graph, tmp_path, 'left-2', 1)
    assert runner._trace_path(tmp_path, 'left', 2, graph) != runner._trace_path(tmp_path, 'left-2', 1, graph)
    assert runner._trace_path(tmp_path, 'a/b', 1, graph) != runner._trace_path(tmp_path, 'a__b', 1, graph)


def test_conversation_history_paths_follow_each_prior_graph_snapshot(tmp_path, monkeypatch):
    raw = definition()
    raw['ops'] = {'split': {'run': 'true'}, 'collect': {'run': 'true'}}
    # Sequential version of the same named worker; no fanout yet.
    raw['nodes'] = [{'id': 'left', 'agent': 'worker'}]
    raw['edges'], raw['entry'] = [], 'left'
    seen = []

    def work(node, directory, options, env, resumed):
        seen.append((node, options.get('previous_steps', ())))
        return success(directory, node)
    workspace, _ = setup(tmp_path, monkeypatch, work, raw)
    assert execute(workspace, conversation_id='conv').status == 'finished'
    (workspace / 'graph.json').write_text(json.dumps(definition()))
    state = runner.run(workspace, config_path='unused', run_id='parallel', conversation_id='conv',
                       previous_runs=(workspace / 'runs/one',))
    assert state.status == 'finished'
    assert next(steps for node, steps in seen if node == 'left' and steps) == (workspace / 'runs/one/control/left',)


def test_process_death_after_native_completion_recovers_same_invocations(tmp_path):
    import os
    import subprocess
    import sys
    from pathlib import Path

    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    (workspace / 'graph.json').write_text(json.dumps(definition()))
    script = tmp_path / 'crash_worker.py'
    script.write_text('''
import json, os, sys, threading
from pathlib import Path
from types import SimpleNamespace
from anchor.simple import run as r
from anchor.node import node_key
from anchor.node.recovery import CompletionFact, record_completion
workspace = Path(sys.argv[1])
resume = len(sys.argv) > 2
r._config = lambda _: ({}, None)
barrier = threading.Barrier(2)
def factory(graph, node, directory, *args, **options):
    class Worker:
        env = SimpleNamespace(route=None)
        def run(self, **kw):
            with (workspace / (node + '.calls')).open('a') as f:
                f.write('called\\n')
            (directory / 'result.txt').write_text(node)
            if node in ('left', 'right'):
                record_completion(options['control'], CompletionFact(node=node_key(node), run=node,
                    kind='agent', submission=node, route=None, command='', at='now'))
                barrier.wait(10)
                if not resume:
                    os._exit(73)
            return {'submission': node, 'exit_status': 'Submitted'}
    return Worker()
r._agent_for = factory
state = r.run(workspace, config_path='unused', run_id='crashed',
    resume=workspace / 'runs/crashed' if resume else None)
assert state.status == 'finished'
''')
    env = {**os.environ, 'PYTHONPATH': str(Path(__file__).resolve().parents[1] / 'src')}
    child = subprocess.run([sys.executable, str(script), str(workspace)], env=env,
                            capture_output=True, text=True, timeout=30)
    assert child.returncode == 73, child.stdout + child.stderr
    state = runner.RunState.load(workspace / 'runs/crashed')
    assert set(state.active) == {'left', 'right'} and state.parallel['completed'] == {}
    resumed = subprocess.run([sys.executable, str(script), str(workspace), 'resume'], env=env,
                              capture_output=True, text=True, timeout=30)
    assert resumed.returncode == 0, resumed.stdout + resumed.stderr
    for node in ('left', 'right', 'after'):
        assert (workspace / (node + '.calls')).read_text() == 'called\n'
    assert len(list((workspace / 'runs').iterdir())) == 1
