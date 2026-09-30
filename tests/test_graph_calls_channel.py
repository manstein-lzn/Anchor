"""Background Graph calls arbitrate an existing channel conversation, never a Pilot turn."""
import json
import threading

import pytest

from anchor.channel import background
from anchor.channel.supervisor import ChannelSupervisor
from anchor.library import Library
from anchor.serve import Scheduler
from anchor.simple import run as runner


def setup(tmp_path, monkeypatch):
    monkeypatch.setenv('ANCHOR_WECOM_USERS', 'alice')
    monkeypatch.delenv('ANCHOR_WECOM_SEND_USERS', raising=False)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    for name in ('source', 'assistant'):
        workspace = tmp_path / 'workspaces' / name
        workspace.mkdir(parents=True)
        (workspace / 'graph.json').write_text(json.dumps({
            'objective': 'test', 'ops': {'reply': {'run': 'true'}},
            'nodes': [{'id': 'reply', 'op': 'reply'}], 'edges': []}))
    session = scheduler.sessions.create('alice-session', graph='assistant', reply_node='reply',
                                       channel={'source': 'wecom', 'sender_id': 'alice', 'conversation_id': 'alice'})
    source = scheduler.workspace('source')
    (source / 'runs' / 'parent').mkdir(parents=True, exist_ok=True)
    runner.RunState(objective='fixture', started='2026-09-30', trigger={'source': 'manual'}).save(source / 'runs' / 'parent')
    spec = {'graph': 'assistant', 'mode': 'detach', 'session': session.id}
    context = background.validate(scheduler, source, 'parent', spec)
    record = {'graph': 'assistant', 'run': 'child', 'spec': spec, 'session_context': context,
              'session_pending': True, 'trigger': {'source': 'graph_call', 'run': 'parent', 'session': session.id}}
    workspace = scheduler.workspace('assistant')
    run_dir = workspace / 'runs' / 'child'
    run_dir.mkdir(parents=True, exist_ok=True)
    state = runner.RunState(objective='fixture', started='2026-09-30', trigger=record['trigger'])
    state.save(run_dir)
    (run_dir / 'admission.json').write_text(json.dumps(record))
    return scheduler, record, workspace, run_dir


def test_background_waits_yields_and_resumes_same_run(tmp_path, monkeypatch):
    scheduler, record, workspace, run_dir = setup(tmp_path, monkeypatch)
    foreground_done = threading.Event()
    scheduler.channel_tail['alice-session'] = ('foreground', foreground_done)
    started, stopped, second = threading.Event(), threading.Event(), threading.Event()
    calls = []

    def execute(workspace, **options):
        calls.append(options)
        state = runner.RunState.load(run_dir)
        if len(calls) == 1:
            started.set()
            while options['stop_request']() != 'stopped':
                stopped.wait(.01)
            state.status = 'stopped'
            state.save(run_dir)
            stopped.set()
        else:
            state.status = 'finished'
            state.save(run_dir)
            second.set()
        return state

    monkeypatch.setattr(background.runner, 'run', execute)
    deliveries = []
    monkeypatch.setattr(background, '_deliver', lambda *args: deliveries.append(args))
    output = []
    worker = threading.Thread(target=lambda: output.append(background.execute(
        scheduler, record, workspace, 'child', {'stop_request': lambda: None})))
    worker.start()
    assert not started.wait(.15)
    with scheduler.lock:
        scheduler.channel_tail.clear()
        foreground_done.set()
    assert started.wait(2)
    with scheduler.lock:
        lease = scheduler.session_background['alice-session']
        next_user_done = threading.Event()
        scheduler.channel_tail['alice-session'] = ('next-user', next_user_done)
        lease.interrupt()
    assert stopped.wait(2)
    assert lease.released.wait(2)
    assert not second.wait(.15)
    with scheduler.lock:
        scheduler.channel_tail.clear()
        next_user_done.set()
    worker.join(3)
    assert not worker.is_alive()
    assert output[0].status == 'finished'
    assert len(calls) == 2 and len(deliveries) == 1
    assert all(c['resume'] == run_dir and not c['preserve_interrupted'] for c in calls)
    assert not json.loads((run_dir / 'admission.json').read_text())['session_pending']
    assert scheduler.sessions.get('alice-session').run_ids == ['child']


def test_session_call_denies_cross_user_and_wrong_graph(tmp_path, monkeypatch):
    scheduler, record, workspace, run_dir = setup(tmp_path, monkeypatch)
    source = scheduler.workspace('source')
    (source / 'runs' / 'parent').mkdir(parents=True, exist_ok=True)
    runner.RunState(objective='fixture', started='2026-09-30', trigger={'source': 'channel', 'session': 'bob-session'}).save(source / 'runs' / 'parent')
    with pytest.raises(ValueError, match="another user's"):
        background.validate(scheduler, source, 'parent', record['spec'])
    with pytest.raises(ValueError, match='bound'):
        background.validate(scheduler, source, 'parent', {**record['spec'], 'graph': 'source'})


def test_multiple_graphs_share_one_plugin_channel(tmp_path):
    plugin = tmp_path / 'library/plugins/transport'
    plugin.mkdir(parents=True)
    (plugin / 'plugin.json').write_text(json.dumps({'name': 'transport', 'description': 'fixture'}))
    (plugin / 'channel.json').write_text(json.dumps({
        'platform': 'wecom', 'transport': 'websocket', 'entrypoint': 'daemon.py',
        'required_environment': []}))
    (plugin / 'daemon.py').write_text('')
    workspaces = []
    for name in ('chat', 'report'):
        workspace = tmp_path / 'workspaces' / name
        workspace.mkdir(parents=True)
        (workspace / 'graph.json').write_text(json.dumps({
            'objective': 'test', 'agents': {'a': {'model': 'default'}},
            'nodes': [{'id': 'a', 'agent': 'a', 'plugins': ['transport']}], 'edges': []}))
        workspaces.append(workspace)
    supervisor = ChannelSupervisor(tmp_path, Library(tmp_path / 'library'), lambda: workspaces,
                                   callback_url='http://localhost', api_key='fake')
    desired = supervisor._desired()
    assert list(desired) == ['wecom']
    assert desired['wecom']['plugin'] == 'transport'


def test_nested_call_cannot_launder_source_session(tmp_path, monkeypatch):
    scheduler, record, workspace, run_dir = setup(tmp_path, monkeypatch)
    source = scheduler.workspace('source')
    runner.RunState(objective='fixture', started='2026-09-30', trigger={
        'source': 'channel', 'session': 'other-user'}).save(source / 'runs' / 'parent')
    helper = source / 'runs' / 'helper'
    helper.mkdir()
    runner.RunState(objective='fixture', started='2026-09-30', trigger={
        'source': 'graph_call', 'graph': 'source', 'run': 'parent'}).save(helper)
    with pytest.raises(ValueError, match="another user's"):
        background.validate(scheduler, source, 'helper', record['spec'])


def test_delivery_failure_is_failed_run_not_finished(tmp_path, monkeypatch):
    scheduler, record, workspace, run_dir = setup(tmp_path, monkeypatch)
    def finish(*args, **kwargs):
        state = runner.RunState.load(run_dir)
        state.status = 'finished'
        state.save(run_dir)
        return state
    monkeypatch.setattr(background.runner, 'run', finish)
    def fail(*args):
        raise RuntimeError('ACK unknown; no retry')
    monkeypatch.setattr(background, '_deliver', fail)
    with pytest.raises(RuntimeError, match='ACK unknown'):
        background.execute(scheduler, record, workspace, 'child', {})
    state = runner.RunState.load(run_dir)
    assert state.status == 'failed' and 'ACK unknown' in state.error
    assert not json.loads((run_dir / 'admission.json').read_text())['session_pending']
    assert not scheduler.session_background


def test_pending_projection_distinguishes_same_session_running_and_queued(tmp_path, monkeypatch):
    scheduler, record, workspace, run_dir = setup(tmp_path, monkeypatch)
    source_dir = scheduler.workspace('source') / 'runs' / 'parent'
    for index, identifier in enumerate(('child', 'queued'), 1):
        target = workspace / 'runs' / identifier
        target.mkdir(exist_ok=True)
        item = {**record, 'run': identifier, 'node': 'notify', 'invocation': index, 'mode': 'detach'}
        runner.RunState(objective='fixture', started='2026-09-30', trigger=item['trigger']).save(target)
        (target / 'admission.json').write_text(json.dumps(item))
        control = source_dir / 'control' / str(index)
        control.mkdir(parents=True)
        (control / 'graph-call.json').write_text(json.dumps(item))
    scheduler.session_background['alice-session'] = background._Lease(run_id='child')
    items = scheduler.graph_calls.projections(source_dir)
    assert [(item['run'], item['status']) for item in items] == [('child', 'running'), ('queued', 'queued')]
