"""Session-call coordination through Rust ports and the existing gateway ledger."""

from contextlib import contextmanager
import copy
import hashlib
import json
import threading
from types import SimpleNamespace

import pytest

from anchor.channel import ChannelEvent, EventLedger
from anchor.channel import runtime_background as background
from anchor.runtime_http import RuntimeHTTPError


class Sessions:
    def __init__(self):
        self.session = SimpleNamespace(
            id='alice-session', status='active', graph='assistant', reply_node='reply',
            conversation_id='alice-conversation', channel={'source': 'wecom', 'sender_id': 'alice'},
            run_ids=[])
        self.event_records = []
        self.started_error = None

    def get(self, identifier):
        if identifier != self.session.id:
            raise KeyError(identifier)
        return copy.deepcopy(self.session)

    def attach_run(self, identifier, run):
        assert identifier == self.session.id
        if run not in self.session.run_ids:
            self.session.run_ids.append(run)

    def append(self, identifier, kind, data):
        assert identifier == self.session.id
        if kind == 'graph.call.started' and self.started_error is not None:
            raise self.started_error
        self.event_records.append(SimpleNamespace(kind=kind, data=copy.deepcopy(data)))

    def events(self, identifier):
        assert identifier == self.session.id
        return list(self.event_records)


class Runtime:
    def __init__(self):
        self.lock = threading.RLock()
        self.records = {}
        self.requests = []
        self.invocations = []
        self.execution_status = 'completed'
        self.reply = {'submitted': True, 'submission': 'Background reply'}
        self.started = threading.Event()
        self.resumed = threading.Event()
        self.yield_requested = threading.Event()
        self.yield_error_seen = threading.Event()
        self.stop_released = threading.Event()
        self.stop_released.set()
        self.settlement_errors = []
        self.execution_errors = []
        self.snapshot_errors = []
        self.commit_before_error = False
        self.yield_errors = []

    def snapshot(self, identifier):
        with self.lock:
            if self.snapshot_errors:
                raise self.snapshot_errors.pop(0)
            record = self.records.get(identifier)
            if record and record['active'] and self.yield_requested.is_set() and self.stop_released.is_set():
                record['active'] = False
                record['state']['status'] = 'stopped'
            return copy.deepcopy(record)

    def request(self, method, path, body=None):
        assert method == 'POST'
        _, runs, identifier, operation = path.split('/')
        assert runs == 'runs'
        with self.lock:
            self.requests.append((identifier, operation, copy.deepcopy(body)))
            record = self.records[identifier]
            if operation == 'session-execution':
                assert body['session'] == 'alice-session'
                if record['state']['status'] != 'completed':
                    self.invocations.append(identifier)
                    record['state']['trigger'].update(
                        session=body['session'], reply_node='reply', previous_run=body['previous_run'])
                    record['state']['status'] = self.execution_status
                    record['state']['nodes']['reply'] = copy.deepcopy(self.reply)
                    record['active'] = self.execution_status == 'running'
                    self.started.set()
                    if len(self.invocations) > 1:
                        self.resumed.set()
                if self.execution_errors:
                    raise self.execution_errors.pop(0)
            elif operation == 'session-yield':
                self.yield_requested.set()
                if self.yield_errors:
                    self.yield_error_seen.set()
                    raise self.yield_errors.pop(0)
                if not record['active']:
                    record['state']['status'] = 'stopped'
            elif operation == 'session-settlement':
                assert not record['active'], 'settlement must wait for the native worker to stop'
                if self.settlement_errors:
                    if self.commit_before_error:
                        record['session_call'].update(body)
                    error = self.settlement_errors.pop(0)
                    if isinstance(error, tuple):
                        return error
                    raise error
                record['session_call'].update(body)
            else:
                raise AssertionError(operation)
        return {'run': identifier}, 200


class Gateway:
    def __init__(self, path):
        self.ledger = EventLedger(path)
        self.requests = []
        self.sends = []
        self.receipts = []
        self.ack_error = None

    def send(self, platform, payload):
        assert platform == 'wecom'
        assert payload['operation'] == 'send'
        self.requests.append(copy.deepcopy(payload))
        event = ChannelEvent(source='wecom-outbound', event_id=payload['request_id'], sender_id='anchor',
                             conversation_id=payload['userid'], text=payload['content'])
        digest = hashlib.sha256(json.dumps([payload['userid'], payload['content']], ensure_ascii=False).encode()).hexdigest()
        receipt = self.ledger.claim_send(event, digest)
        if receipt is None:
            self.sends.append(copy.deepcopy(payload))
            if self.ack_error is not None:
                self.ledger.fail(event, 'delivery uncertain')
                raise self.ack_error
            self.ledger.complete(event)
            receipt = {'accepted': True, 'request_id': payload['request_id']}
        self.receipts.append(receipt)
        return receipt


class Scheduler:
    def __init__(self, path, *, runtime=None, sessions=None):
        self.runtime = runtime or Runtime()
        self.sessions = sessions or Sessions()
        self.channel_supervisor = Gateway(path)
        self.lock = threading.RLock()
        self.channel_tail = {}
        self.session_background = {}
        self.session_call_workers = set()
        self.graph_response = ({'node_plugins': {'reply': ['wecom']}}, 200)
        self.reads = []

    def run(self, graph, identifier):
        assert graph == ''
        self.reads.append(identifier)
        return self.runtime.snapshot(identifier)

    def runs(self):
        return [{'run': value['run'], 'backend': value['backend'], 'trigger': value['state']['trigger'],
                 'session_call': value.get('session_call')}
                for value in copy.deepcopy(self.runtime.records).values()]

    def graph(self, identifier):
        assert identifier == 'assistant'
        value, status = self.graph_response
        return json.dumps(value), status


def record(identifier, graph='source', trigger=None):
    return {'run': identifier, 'graph': graph, 'backend': 'rust', 'active': False,
            'state': {'status': 'stopped', 'trigger': trigger or {'source': 'schedule'}, 'nodes': {}}}


@pytest.fixture
def scheduler(tmp_path, monkeypatch):
    monkeypatch.setenv('ANCHOR_WECOM_SEND_USERS', 'alice')
    monkeypatch.setenv('ANCHOR_WECOM_USERS', 'alice,bob')
    monkeypatch.setattr(background, 'POLL_INTERVAL', .005)
    value = Scheduler(tmp_path / 'gateway.sqlite')
    value.runtime.records['parent'] = record('parent')
    child = record('child', 'assistant', {'source': 'graph_call', 'run': 'parent', 'graph': 'source'})
    child['session_call'] = {'status': 'pending', 'context': background._context(value, 'alice-session', 'assistant')}
    value.runtime.records['child'] = child
    return value


def resolve(scheduler, **updates):
    body, status = background.resolve(scheduler, {'parent_run': 'parent', 'session': 'alice-session',
                                                 'graph': 'assistant', **updates})
    return json.loads(body), status


def execute(scheduler):
    scheduler.session_call_workers.add('child')
    background.execute(scheduler, 'child')


@contextmanager
def worker(scheduler):
    thread = threading.Thread(target=execute, args=(scheduler,), daemon=True)
    thread.start()
    try:
        yield thread
    finally:
        scheduler.runtime.stop_released.set()
        with scheduler.lock:
            for _, completed in scheduler.channel_tail.values():
                completed.set()
            scheduler.channel_tail.clear()
        with scheduler.runtime.lock:
            child = scheduler.runtime.records['child']
            if child['session_call']['status'] == 'pending':
                child['session_call']['status'] = 'failed'
            child['active'] = False
            child['state']['status'] = 'stopped'
        thread.join(2)
        assert not thread.is_alive()


def settlements(scheduler):
    return [body for _, operation, body in scheduler.runtime.requests if operation == 'session-settlement']


def test_resolve_freezes_trusted_context_and_checks_complete_ancestry(scheduler):
    scheduler.runtime.records['helper'] = record('helper', 'helper-graph',
                                                {'source': 'graph_call', 'run': 'parent', 'graph': 'source'})
    value, status = resolve(scheduler, parent_run='helper')
    assert status == 200
    assert value == {'session': 'alice-session', 'reply_node': 'reply', 'conversation_id': 'alice-conversation',
                     'channel': {'source': 'wecom', 'sender_id': 'alice'}}
    assert scheduler.reads == ['helper', 'parent']
    assert not scheduler.runtime.requests


@pytest.mark.parametrize(('change', 'error'), [
    ({'state': {'trigger': {'source': 'channel', 'session': 'other-user'}}}, 'conversation cannot'),
    ({'session_call': {'status': 'pending'}}, 'conversation cannot'),
    ({'graph': 'assistant'}, 'recursive Graph call'),
    ({'backend': 'legacy'}, 'source is missing'),
    ({'state': {'trigger': {'source': 'graph_call', 'run': 'parent'}}}, 'cyclic call ancestry'),
])
def test_resolve_rejects_disallowed_ancestors(scheduler, change, error):
    scheduler.runtime.records['parent'].update(change)
    scheduler.runtime.records['helper'] = record('helper', 'helper-graph',
                                                {'source': 'graph_call', 'run': 'parent'})
    value, status = resolve(scheduler, parent_run='helper')
    assert status == 400 and error in value['error']


@pytest.mark.parametrize(('field', 'value', 'error'), [
    ('status', 'archived', 'active channel conversation'),
    ('graph', 'other-graph', 'active channel conversation'),
    ('channel', {}, 'active channel conversation'),
    ('channel', {'source': 'other', 'sender_id': 'alice'}, 'unsupported session channel'),
    ('channel', {'source': 'wecom', 'sender_id': 'bob'}, 'recipient is not allowed'),
    ('reply_node', 'missing', 'reply node no longer exists'),
])
def test_resolve_rechecks_session_binding_and_recipient(scheduler, field, value, error):
    setattr(scheduler.sessions.session, field, value)
    response, status = resolve(scheduler)
    assert status == 400 and error in response['error']


@pytest.mark.parametrize('body', [None, {}, {'parent_run': '', 'session': 'alice-session', 'graph': 'assistant'},
                                 {'parent_run': [], 'session': 'alice-session', 'graph': 'assistant'},
                                 {'parent_run': 'parent', 'session': 'alice-session', 'graph': 'assistant', 'input': {}}])
def test_resolve_rejects_invalid_request_without_ancestry_bypass(scheduler, body):
    value, status = background.resolve(scheduler, body)
    assert status == 400 and 'invalid Session call request' in json.loads(value)['error']
    assert not scheduler.reads


def test_resolve_missing_parent_and_runtime_errors(scheduler):
    del scheduler.runtime.records['parent']
    value, status = resolve(scheduler)
    assert status == 400 and 'source is missing' in value['error']
    scheduler.graph_response = ({'error': 'Runtime unavailable'}, 503)
    value, status = resolve(scheduler)
    assert status == 503 and value['backend'] == 'rust'


def test_execute_binds_latest_predecessor_and_delivers_same_child(scheduler):
    first = record('first', 'assistant', {'source': 'channel', 'session': 'alice-session',
                                         'reply_node': 'reply', 'previous_run': None})
    second = copy.deepcopy(first)
    second['run'] = 'second'
    second['state']['trigger']['previous_run'] = 'first'
    scheduler.runtime.records.update(first=first, second=second)
    execute(scheduler)
    assert scheduler.runtime.records['child']['state']['trigger']['previous_run'] == 'second'
    assert scheduler.runtime.records['child']['session_call']['status'] == 'delivered'
    assert scheduler.sessions.session.run_ids == ['child']
    key = hashlib.sha256(b'graph-call-reply:child').hexdigest()
    assert scheduler.channel_supervisor.sends == [
        {'operation': 'send', 'request_id': key, 'userid': 'alice', 'content': 'Background reply'}]
    assert scheduler.runtime.invocations == ['child']
    assert settlements(scheduler) == [{'status': 'delivered'}]
    assert not scheduler.session_background and not scheduler.session_call_workers


@pytest.mark.parametrize('error', [RuntimeHTTPError('settlement response interrupted'),
                                 ({'error': 'settlement unavailable'}, 503), OSError('connection lost')])
def test_ack_then_failed_settlement_retries_existing_gateway_receipt(scheduler, tmp_path, error):
    scheduler.runtime.settlement_errors = [error]
    execute(scheduler)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'pending'
    assert settlements(scheduler) == [{'status': 'delivered'}]
    assert len(scheduler.channel_supervisor.sends) == 1
    assert not scheduler.session_background and not scheduler.session_call_workers
    restarted = Scheduler(tmp_path / 'gateway.sqlite', runtime=scheduler.runtime, sessions=scheduler.sessions)
    execute(restarted)
    assert restarted.runtime.records['child']['session_call']['status'] == 'delivered'
    assert settlements(restarted) == [{'status': 'delivered'}, {'status': 'delivered'}]
    assert restarted.channel_supervisor.sends == []
    assert restarted.channel_supervisor.receipts == []
    assert restarted.channel_supervisor.requests == []
    assert restarted.runtime.invocations == ['child']
    assert set(restarted.runtime.records) == {'parent', 'child'}


def test_lost_response_after_durable_delivery_never_settles_failed(scheduler):
    scheduler.runtime.settlement_errors = [RuntimeHTTPError('lost committed settlement response')]
    scheduler.runtime.commit_before_error = True
    execute(scheduler)
    execute(scheduler)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'delivered'
    assert settlements(scheduler) == [{'status': 'delivered'}]
    assert len(scheduler.channel_supervisor.sends) == 1


def test_persisted_ack_settles_after_restart_without_current_recipient_access(scheduler, tmp_path):
    scheduler.runtime.settlement_errors = [RuntimeHTTPError('lost committed settlement response')]
    scheduler.runtime.commit_before_error = False
    execute(scheduler)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'pending'
    assert len([event for event in scheduler.sessions.events('alice-session')
                if event.kind == 'graph.call.delivered']) == 1

    scheduler.sessions.session.status = 'archived'
    scheduler.sessions.session.channel = {'source': 'wecom', 'sender_id': 'revoked-user'}
    restarted = Scheduler(tmp_path / 'gateway.sqlite', runtime=scheduler.runtime, sessions=scheduler.sessions)
    execute(restarted)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'delivered'
    assert restarted.channel_supervisor.requests == []
    assert restarted.channel_supervisor.sends == []
    assert settlements(restarted)[-1:] == [{'status': 'delivered'}]


def test_transient_run_read_failure_stays_pending_without_failed_settlement(scheduler):
    scheduler.runtime.snapshot_errors = [RuntimeHTTPError('Runtime unavailable', 503)]
    execute(scheduler)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'pending'
    assert scheduler.runtime.requests == []
    assert not scheduler.session_call_workers
    execute(scheduler)
    assert settlements(scheduler) == [{'status': 'delivered'}]


def test_lost_execution_response_waits_for_inactivity_then_retries_same_child(scheduler):
    scheduler.runtime.execution_errors = [RuntimeHTTPError('execution response interrupted', 503)]
    scheduler.runtime.execution_status = 'running'
    execute(scheduler)
    assert scheduler.runtime.records['child']['session_call']['status'] == 'pending'
    assert scheduler.runtime.records['child']['active'] is False
    assert settlements(scheduler) == []
    assert not scheduler.session_background and not scheduler.session_call_workers
    scheduler.runtime.execution_status = 'completed'
    execute(scheduler)
    assert settlements(scheduler) == [{'status': 'delivered'}]
    assert scheduler.runtime.invocations == ['child', 'child']
    assert set(scheduler.runtime.records) == {'parent', 'child'}


def test_foreground_interrupts_then_background_resumes_original_child(scheduler):
    scheduler.runtime.execution_status = 'running'
    scheduler.runtime.stop_released.clear()
    prior_done = threading.Event()
    scheduler.channel_tail['alice-session'] = ('prior-turn', prior_done)
    with worker(scheduler) as thread:
        assert not scheduler.runtime.started.wait(.03)
        with scheduler.lock:
            scheduler.channel_tail.clear()
            prior_done.set()
        assert scheduler.runtime.started.wait(2)
        with scheduler.lock:
            lease = scheduler.session_background['alice-session']
            lease.interrupt()
            foreground_done = threading.Event()
            scheduler.channel_tail['alice-session'] = ('new-turn', foreground_done)
        assert scheduler.runtime.yield_requested.wait(2)
        assert not lease.released.wait(.03)
        assert scheduler.runtime.snapshot('child')['active'] is True
        scheduler.runtime.stop_released.set()
        assert lease.released.wait(2)
        assert not scheduler.runtime.resumed.wait(.03)
        assert scheduler.channel_supervisor.sends == []
        with scheduler.lock:
            scheduler.runtime.execution_status = 'completed'
            scheduler.channel_tail.clear()
            foreground_done.set()
        assert scheduler.runtime.resumed.wait(2)
        thread.join(2)
        assert not thread.is_alive()
        assert scheduler.runtime.invocations == ['child', 'child']
        assert set(scheduler.runtime.records) == {'parent', 'child'}
        assert settlements(scheduler) == [{'status': 'delivered'}]
        assert len(scheduler.channel_supervisor.sends) == 1


def test_setup_error_and_failed_yield_cannot_release_active_session(scheduler):
    scheduler.runtime.execution_status = 'running'
    scheduler.runtime.stop_released.clear()
    scheduler.runtime.yield_errors = [RuntimeHTTPError('yield response lost')]
    scheduler.sessions.started_error = RuntimeError('fixture Session append failed')
    with worker(scheduler) as thread:
        assert scheduler.runtime.started.wait(2)
        assert scheduler.runtime.yield_error_seen.wait(2)
        with scheduler.lock:
            lease = scheduler.session_background['alice-session']
            lease.interrupt()
            scheduler.channel_tail['alice-session'] = ('new-turn', threading.Event())
        # A completed projection still must not release an active native worker.
        with scheduler.runtime.lock:
            scheduler.runtime.records['child']['state']['status'] = 'completed'
        assert not lease.released.wait(.03)
        assert settlements(scheduler) == []
        scheduler.runtime.stop_released.set()
        assert lease.released.wait(2)
        assert scheduler.runtime.snapshot('child')['active'] is False
        thread.join(2)
        assert not thread.is_alive()
        assert settlements(scheduler) == [{'status': 'failed', 'error': 'fixture Session append failed'}]
        assert not scheduler.channel_supervisor.sends


@pytest.mark.parametrize('status', ['failed', 'aborted', 'budget_stopped', 'stopped'])
def test_execution_errors_settle_failed_without_delivery(scheduler, status):
    scheduler.runtime.execution_status = status
    execute(scheduler)
    assert settlements(scheduler) == [{'status': 'failed', 'error': 'background execution ' + status}]
    assert not scheduler.channel_supervisor.sends
    assert not scheduler.session_background and not scheduler.session_call_workers


@pytest.mark.parametrize('reply', [{'submitted': False, 'submission': 'reply'},
                                  {'submitted': True, 'submission': ' '},
                                  {'submitted': True, 'submission': 10}])
def test_invalid_reply_is_failed_before_gateway_send(scheduler, reply):
    scheduler.runtime.reply = reply
    execute(scheduler)
    assert settlements(scheduler) == [{'status': 'failed', 'error': 'the assistant reply node did not produce a reply'}]
    assert not scheduler.channel_supervisor.sends


def test_changed_context_is_rejected_before_execution(scheduler):
    scheduler.sessions.session.conversation_id = 'replacement-conversation'
    execute(scheduler)
    assert scheduler.runtime.invocations == []
    assert settlements(scheduler) == [
        {'status': 'failed', 'error': 'the bound channel conversation changed after admission'}]
    assert not scheduler.channel_supervisor.sends


def test_parent_stop_after_completed_observation_suppresses_delivery(scheduler, monkeypatch):
    context = background._context
    checks = []

    def stop_before_final_read(*args):
        value = context(*args)
        checks.append(value)
        if len(checks) == 2:
            scheduler.runtime.records['child']['session_call']['status'] = 'failed'
        return value

    monkeypatch.setattr(background, '_context', stop_before_final_read)
    execute(scheduler)
    assert len(checks) == 2
    assert scheduler.runtime.records['child']['session_call']['status'] == 'failed'
    assert settlements(scheduler) == []
    assert not scheduler.channel_supervisor.sends


def test_unknown_gateway_ack_is_failed_and_never_automatically_replayed(scheduler):
    scheduler.channel_supervisor.ack_error = RuntimeError('delivery not confirmed; do not resend automatically')
    execute(scheduler)
    execute(scheduler)
    assert settlements(scheduler) == [
        {'status': 'failed', 'error': 'delivery not confirmed; do not resend automatically'}]
    assert len(scheduler.channel_supervisor.requests) == 1
    assert len(scheduler.channel_supervisor.sends) == 1


def test_context_change_before_delivery_fails_without_gateway_send(scheduler, monkeypatch):
    context = background._context
    checks = []

    def rebind(*args):
        checks.append(args)
        if len(checks) == 2:
            scheduler.sessions.session.conversation_id = 'replacement-conversation'
        return context(*args)

    monkeypatch.setattr(background, '_context', rebind)
    execute(scheduler)
    assert settlements(scheduler) == [
        {'status': 'failed', 'error': 'the bound channel conversation changed before delivery'}]
    assert not scheduler.channel_supervisor.sends


def test_ambiguous_conversation_predecessors_fail_before_execution(scheduler):
    trigger = {'source': 'channel', 'session': 'alice-session', 'reply_node': 'reply', 'previous_run': None}
    scheduler.runtime.records.update(first=record('first', 'assistant', trigger),
                                     second=record('second', 'assistant', trigger))
    execute(scheduler)
    assert settlements(scheduler) == [{'status': 'failed', 'error': 'conversation lineage has multiple heads'}]
    assert scheduler.runtime.invocations == []
    assert not scheduler.channel_supervisor.sends


def test_tick_claims_only_pending_rust_calls_once(scheduler, monkeypatch):
    threads = []
    monkeypatch.setattr(background.threading, 'Thread',
                        lambda **kwargs: SimpleNamespace(start=lambda: threads.append(kwargs)))
    scheduler.runtime.records['legacy'] = {**copy.deepcopy(scheduler.runtime.records['child']),
                                           'run': 'legacy', 'backend': 'legacy'}
    scheduler.runtime.records['delivered'] = {**copy.deepcopy(scheduler.runtime.records['child']), 'run': 'delivered',
                                              'session_call': {'status': 'delivered'}}
    background.tick(scheduler)
    background.tick(scheduler)
    assert scheduler.session_call_workers == {'child'}
    assert len(threads) == 1
    assert threads[0] == {'target': background.execute, 'args': (scheduler, 'child'), 'daemon': True}
