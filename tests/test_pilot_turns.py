import asyncio
import json
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer

import pytest
from pydantic_ai import Agent, DeferredToolRequests
from pydantic_ai.messages import ToolReturnPart
from pydantic_ai.models.function import FunctionModel, DeltaToolCall

from anchor import pilot
from anchor.pilot import _register_tools
from anchor.pilot_turns import TurnStore
from anchor.serve import Handler, Scheduler


def test_stream_reads_committed_events_while_writer_holds_transaction(tmp_path):
    store = TurnStore(tmp_path)
    turn, _ = store.create('s', 'r', 'hi')
    store.append(turn['id'], {'type': 'text-delta', 'delta': 'saved'})
    with ThreadPoolExecutor(max_workers=1) as pool, store.connect() as writer:
        writer.execute('BEGIN EXCLUSIVE')
        writer.execute('UPDATE turns SET error=? WHERE id=?', ('uncommitted', turn['id']))
        try:
            events = pool.submit(store.events, 's', turn['id']).result(timeout=2)
            assert events[0]['data']['delta'] == 'saved'
            assert pool.submit(store.get, 's', turn['id']).result(timeout=2)['error'] == ''
        finally:
            writer.rollback()


def test_turn_admission_is_atomic_and_survives_restart(tmp_path):
    store = TurnStore(tmp_path)
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda _: TurnStore(tmp_path).create('s', 'request', 'hi'), range(8)))
    assert sum(created for _, created in results) == 1
    assert len({turn['id'] for turn, _ in results}) == 1
    turn = results[0][0]
    with pytest.raises(ValueError, match='different input'):
        store.create('s', 'request', 'changed')
    with pytest.raises(ValueError, match='already processing'):
        store.create('s', 'next', 'hi')
    with pytest.raises(KeyError):
        store.events('another-session', turn['id'])
    store.append(turn['id'], {'type': 'text-delta', 'id': 'text', 'delta': 'hello'})
    assert store.interrupt_running() == ['s']
    assert store.get('s', turn['id'])['status'] == 'interrupted'
    assert store.events('s', turn['id'])[0]['data']['delta'] == 'hello'
    assert store.events('s', turn['id'], store.events('s', turn['id'])[0]['seq']) == []
    store.create('s', 'next', 'hi')


def test_real_http_stream_reconnect_dedup_and_framework_tool_events(tmp_path, monkeypatch):
    release = threading.Event()
    calls = []
    tools = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(name='read_state', json_args='{}', tool_call_id='call-1')}
        else:
            yield '你好'
            await asyncio.to_thread(release.wait)
            yield '，已读取。'

    def agent(_):
        result = Agent(FunctionModel(stream_function=stream), output_type=str)
        @result.tool_plain
        def read_state():
            tools.append('read')
            return {'ok': True}
        return result

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    scheduler.create_session('http')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)
    feed = HTTPConnection(*server.server_address, timeout=10)
    try:
        body = json.dumps({'request_id': 'request-1', 'message': '你好'})
        def submit():
            client.request('POST', '/sessions/http/turns', body, {'Content-Type': 'application/json'})
            response = client.getresponse()
            data = json.loads(response.read())
            assert response.status == 202, data
            return data['turn']
        turn = submit()
        assert submit()['id'] == turn['id']
        url = f'/sessions/http/turns/{turn["id"]}/events'
        feed.request('GET', url)
        response = feed.getresponse()
        assert response.status == 200
        chunks = []
        cursor = 0
        while True:
            line = response.readline().decode().strip()
            if line.startswith('id: '):
                cursor = int(line[4:])
            if line.startswith('data: '):
                value = json.loads(line[6:])
                chunks.append(value)
                if value.get('type') == 'text-delta':
                    assert value['delta'] == '你好'
                    break
        assert scheduler.turns.get('http', turn['id'])['status'] == 'running'
        assert any(c['type'] == 'tool-input-available' and c['toolCallId'] == 'call-1' for c in chunks)
        assert any(c['type'] == 'tool-output-available' for c in chunks)
        response.close()
        feed.close()
        release.set()
        feed = HTTPConnection(*server.server_address, timeout=10)
        feed.request('GET', url, headers={'Last-Event-ID': str(cursor)})
        response = feed.getresponse()
        remaining = response.read().decode()
        assert '，已读取。' in remaining
        assert 'event: turn' in remaining and '"status": "completed"' in remaining
        assert '"delta": "你好"' not in remaining
        assert tools == ['read'] and len(calls) == 2
        history = json.loads(scheduler.pilot_messages('http')[0])['messages']
        assert history[-1]['text'] == '你好，已读取。'
        assert submit()['id'] == turn['id']
        scheduler.create_session('other')
        feed.request('GET', f'/sessions/other/turns/{turn["id"]}/events')
        response = feed.getresponse()
        assert response.status == 404
        response.read()
        feed.request('GET', '/sessions/other/turns')
        response = feed.getresponse()
        assert response.status == 200 and json.loads(response.read()) == {'turns': []}
        scheduler.sessions.set_status('http', 'archived')
        # Replaying an accepted request stays idempotent; a new submit into an archived session does not.
        assert submit()['id'] == turn['id']
        client.request('POST', '/sessions/http/turns', json.dumps({'request_id': 'request-2', 'message': 'hi'}),
                       {'Content-Type': 'application/json'})
        response = client.getresponse()
        assert response.status == 409
        response.read()
        feed.request('GET', url + '?after=invalid')
        response = feed.getresponse()
        assert response.status == 400
        response.read()
    finally:
        release.set()
        client.close()
        feed.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_stopping_a_turn_keeps_partial_output_and_frees_the_session(tmp_path, monkeypatch):
    started = threading.Event()
    release = threading.Event()
    calls = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield '部分输出'
            started.set()
            await asyncio.to_thread(release.wait)
            yield '停止后不该出现'
        else:
            yield '继续完成'

    monkeypatch.setattr(pilot, '_agent', lambda _: Agent(FunctionModel(stream_function=stream), output_type=str))
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    scheduler.create_session('stop-turn')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)

    def call(method, path, body=None):
        payload = json.dumps(body) if body is not None else None
        client.request(method, path, payload, {'Content-Type': 'application/json'} if payload else {})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    def settled():
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            turn = scheduler.turns.get('stop-turn', started_turn['id'])
            if turn['status'] != 'running':
                return turn
            time.sleep(0.05)
        raise AssertionError('the stopped turn never reached a terminal state')

    started_turn: dict = {}
    try:
        status, data = call('POST', '/sessions/stop-turn/turns', {'request_id': 'r1', 'message': '开始'})
        assert status == 202, data
        started_turn = data['turn']
        assert started.wait(30), 'Pilot never reached the model'
        # The partial answer is durable before the stop, so a refresh mid-cancel still shows it.
        assert [event['data'].get('delta') for event in scheduler.turns.events('stop-turn', started_turn['id'])
                if event['data'].get('type') == 'text-delta'] == ['部分输出']
        assert call('POST', '/sessions/stop-turn/stop')[0] == 202
        release.set()
        stopped = settled()
        assert stopped['status'] == 'stopped'
        assert scheduler.sessions.get('stop-turn').status == 'interrupted'
        assert '停止后不该出现' not in json.dumps(scheduler.turns.events('stop-turn', started_turn['id']), ensure_ascii=False)
        # A stopped turn is not silently replaced, but the session is not closed for business either:
        # the user's next message is a new turn that carries what the stopped attempt left behind.
        status, data = call('POST', '/sessions/stop-turn/turns', {'request_id': 'r2', 'message': '新问题'})
        assert status == 202, data
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and scheduler.turns.get('stop-turn', data['turn']['id'])['status'] == 'running':
            time.sleep(0.05)
        assert scheduler.turns.get('stop-turn', data['turn']['id'])['status'] == 'completed'
        assert json.loads(scheduler.pilot_messages('stop-turn')[0])['messages'][-1]['text'] == '继续完成'
        # The model was given the interrupted attempt, not only the new question.
        assert '部分输出' in json.dumps([[part for part in message.parts] for message in calls[1]],
                                      ensure_ascii=False, default=str)
    finally:
        release.set()
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_a_run_starts_without_a_second_confirmation_and_is_reported(tmp_path, monkeypatch):
    """The user asked for the Run; the tool does it. No per-call gate, no waiting session."""
    calls = []
    started = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(name='graph_run', json_args='{"graph":"demo"}', tool_call_id='call-1')}
        else:
            yield '已启动 demo'

    def agent(_):
        built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(built)
        return built

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')

    def fake_trigger(graph, objective):
        started.append(graph)
        return json.dumps({'run': 'run-1', 'graph': graph}), 202

    monkeypatch.setattr(scheduler, 'trigger', fake_trigger)
    scheduler.create_session('direct-run')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)

    def call(method, path, body=None):
        payload = json.dumps(body) if body is not None else None
        client.request(method, path, payload, {'Content-Type': 'application/json'} if payload else {})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    def settle(turn_id):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            turn = scheduler.turns.get('direct-run', turn_id)
            if turn['status'] != 'running':
                return turn
            time.sleep(0.05)
        raise AssertionError('the turn never reached a terminal state')

    try:
        status, data = call('POST', '/sessions/direct-run/turns',
                            {'request_id': 'r1', 'message': '启动 demo'})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'completed'
        assert started == ['demo'], 'the Run the user asked for must start exactly once'
        session = scheduler.sessions.get('direct-run')
        assert session.status == 'active' and session.run_ids == ['run-1'] and session.approvals == []
        assert json.loads(scheduler.pilot_messages('direct-run')[0])['messages'][-1]['text'] == '已启动 demo'
        assert session.operations['call-1']['status'] == 'completed'
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_a_deleting_turn_pauses_for_the_user_and_resumes_the_same_tool_call(tmp_path, monkeypatch):
    calls = []
    deleted = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(name='graph_delete', json_args='{"graph":"demo"}',
                                    tool_call_id='call-1')}
        else:
            yield '已删除 demo'

    def agent(_):
        built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(built)
        return built

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    monkeypatch.setattr(scheduler, 'delete_graph',
                        lambda graph: (deleted.append(graph),
                                       json.dumps({'graph': graph, 'deleted': True}), 200)[1:])

    scheduler.create_session('approve')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)

    def call(method, path, body=None):
        payload = json.dumps(body) if body is not None else None
        client.request(method, path, payload, {'Content-Type': 'application/json'} if payload else {})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    def settle(turn_id):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            turn = scheduler.turns.get('approve', turn_id)
            if turn['status'] != 'running':
                return turn
            time.sleep(0.05)
        raise AssertionError('the turn never reached a terminal state')

    try:
        status, data = call('POST', '/sessions/approve/turns',
                            {'request_id': 'r1', 'message': '删除 demo'})
        assert status == 202, data
        paused = settle(data['turn']['id'])
        assert paused['status'] == 'waiting_approval'
        assert deleted == [], 'the Graph must not be deleted before the user decides'
        session = scheduler.sessions.get('approve')
        assert session.status == 'waiting_user'
        pending, = session.approvals
        assert (pending['action'], pending['target']) == ('graph_delete', 'demo')
        assert pending['proposal'] == {'graph': 'demo'}
        status, refused = call('POST', '/sessions/approve/turns',
                               {'request_id': 'r-other', 'message': '顺便做点别的'})
        assert status == 409 and 'pending operation' in refused['error']

        status, _ = call('POST', '/sessions/approve/confirm',
                         {'action': 'graph_delete', 'approval_key': pending['key']})
        assert status == 200
        status, data = call('POST', '/sessions/approve/turns', {'request_id': 'r2', 'resume': True})
        assert status == 202, data
        settled = settle(data['turn']['id'])
        assert settled['status'] == 'completed'
        assert deleted == ['demo']
        assert scheduler.sessions.get('approve').approvals == []
        assert json.loads(scheduler.pilot_messages('approve')[0])['messages'][-1]['text'] == '已删除 demo'
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_session_ask_pauses_the_run_until_the_user_answers(tmp_path, monkeypatch):
    calls = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(name='session_ask', json_args='{"question":"研究范围？"}',
                                    tool_call_id='ask-1')}
        else:
            yield '按你的回答继续'

    def agent(_):
        built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(built)
        return built

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    scheduler.create_session('ask')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)

    def call(method, path, body=None):
        payload = json.dumps(body) if body is not None else None
        client.request(method, path, payload, {'Content-Type': 'application/json'} if payload else {})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    def settle(turn_id):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            turn = scheduler.turns.get('ask', turn_id)
            if turn['status'] != 'running':
                return turn
            time.sleep(0.05)
        raise AssertionError('the turn never reached a terminal state')

    try:
        status, data = call('POST', '/sessions/ask/turns',
                            {'request_id': 'r1', 'message': '帮我规划'})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'waiting_user'
        session = scheduler.sessions.get('ask')
        assert session.status == 'waiting_user' and session.waiting_reason == '研究范围？'
        assert scheduler.sessions.pending_question('ask')['tool_call_id'] == 'ask-1'
        assert len(calls) == 1, 'the run must end at the question, not keep calling tools'

        status, data = call('POST', '/sessions/ask/turns',
                            {'request_id': 'r2', 'message': '只看 2020 年后的'})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'completed'
        assert scheduler.sessions.get('ask').questions == []
        # The answer arrives as the tool's result, so it is not a second user turn in the history.
        assert json.loads(scheduler.pilot_messages('ask')[0])['messages'] == [
            {'role': 'user', 'text': '帮我规划'},
            {'role': 'assistant', 'text': '按你的回答继续'},
        ]
        returned = [part for message in calls[1] for part in getattr(message, 'parts', [])
                    if isinstance(part, ToolReturnPart)]
        assert [part.content for part in returned] == ['只看 2020 年后的']
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_a_graph_edited_while_the_user_decides_is_not_deleted(tmp_path, monkeypatch):
    """The deletion was proposed against one version of the file; a newer one needs a new decision."""
    workspace = tmp_path / 'workspaces' / 'demo'
    workspace.mkdir(parents=True)
    graph = workspace / 'graph.json'
    graph.write_text(json.dumps({'nodes': ['original']}), encoding='utf-8')
    calls = []
    deleted = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(name='graph_delete', json_args='{"graph":"demo"}',
                                    tool_call_id='delete-1')}
        else:
            yield '没有删除，图谱已经变了'

    def agent(_):
        built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(built)
        return built

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
    monkeypatch.setattr(scheduler, 'delete_graph',
                        lambda name: (deleted.append(name), json.dumps({'graph': name}), 200)[1:])
    scheduler.create_session('edit')
    handler = type('TestHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = HTTPConnection(*server.server_address, timeout=10)

    def call(method, path, body=None):
        payload = json.dumps(body) if body is not None else None
        client.request(method, path, payload, {'Content-Type': 'application/json'} if payload else {})
        response = client.getresponse()
        return response.status, json.loads(response.read())

    def settle(turn_id):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            turn = scheduler.turns.get('edit', turn_id)
            if turn['status'] != 'running':
                return turn
            time.sleep(0.05)
        raise AssertionError('the turn never reached a terminal state')

    try:
        status, data = call('POST', '/sessions/edit/turns',
                            {'request_id': 'r1', 'message': '删除 demo'})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'waiting_approval'
        pending, = scheduler.sessions.get('edit').approvals
        assert pending['precondition']['graph'] == 'demo'

        # The user edits the same Graph in the canvas while the confirmation is still open.
        graph.write_text(json.dumps({'nodes': ['edited by the user']}), encoding='utf-8')
        status, _ = call('POST', '/sessions/edit/confirm',
                         {'action': 'graph_delete', 'approval_key': pending['key']})
        assert status == 200
        status, data = call('POST', '/sessions/edit/turns', {'request_id': 'r2', 'resume': True})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'completed'
        assert deleted == [], 'a confirmation for an older version must not delete the newer file'
        assert json.loads(graph.read_text(encoding='utf-8'))['nodes'] == ['edited by the user']
        assert json.loads(scheduler.pilot_messages('edit')[0])['messages'][-1]['text'] == '没有删除，图谱已经变了'
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


#: A Pilot process that is killed inside a tool. Run as a child process so the kill is a real one:
#: a caught exception would take a path the framework handles, and this is about the path it cannot.
KILLED_PILOT = '''
import json, os, sys, threading
from pathlib import Path
from pydantic_ai import Agent
from pydantic_ai.messages import ToolCallPart
from pydantic_ai.models.function import DeltaToolCall, FunctionModel
import anchor.pilot as pilot
from anchor.serve import Scheduler

root, config = Path(sys.argv[1]), Path(sys.argv[2])


async def stream(messages, info):
    calls = [part for message in messages for part in getattr(message, "parts", [])
             if isinstance(part, ToolCallPart)]
    if not calls:
        yield {0: DeltaToolCall(name="read_probe", json_args="{}", tool_call_id="call-read")}
    else:
        yield {0: DeltaToolCall(name="slow_probe", json_args="{}", tool_call_id="call-slow")}


def agent(_config):
    built = Agent(FunctionModel(stream_function=stream), output_type=str)

    @built.tool_plain
    def read_probe() -> str:
        """A tool that finishes and whose result is recorded."""
        return "recorded-result"

    @built.tool_plain
    def slow_probe() -> str:
        """A tool the process dies inside."""
        print("TOOL-STARTED", flush=True)
        os.kill(os.getpid(), 9)
        return "never"

    return built


pilot._agent = agent
scheduler = Scheduler(root, config)
scheduler.create_session("killed")
# The same entry point the interface uses, so the kill lands on a real running turn.
turn, created = scheduler.turns.create("killed", "request-1", "先读一下现场")
assert created
threading.Thread(target=scheduler._run_turn, args=(turn,), daemon=True).start()
threading.Event().wait(120)
print("UNREACHABLE", flush=True)
'''


def _kill_a_pilot_process(tmp_path):
    """Run one Pilot turn in a child process and let it die inside a tool."""
    import subprocess
    import sys as _sys
    root = tmp_path / 'data'
    root.mkdir(exist_ok=True)
    config = tmp_path / 'runtime.json'
    config.write_text('{"models": []}', encoding='utf-8')
    script = tmp_path / 'killed_pilot.py'
    script.write_text(KILLED_PILOT, encoding='utf-8')
    finished = subprocess.run([_sys.executable, str(script), str(root), str(config)],
                              capture_output=True, text=True, timeout=120)
    assert finished.returncode == -9, (finished.returncode, finished.stdout, finished.stderr)
    assert 'TOOL-STARTED' in finished.stdout, finished.stdout
    return root, config


def _model_that_reports_what_it_saw(seen):
    from pydantic_ai.messages import ModelResponse, TextPart

    def model(messages, info):
        seen['messages'] = messages
        return ModelResponse(parts=[TextPart(content='我看到了中断，先核查现场。')])
    return model


@pytest.mark.parametrize('prompt', ['现场情况如何？', None])
def test_a_killed_process_is_continued_by_the_next_message(tmp_path, monkeypatch, prompt):
    root, config = _kill_a_pilot_process(tmp_path)
    scheduler = Scheduler(root, config)
    assert scheduler.turns.list('killed')[0]['status'] == 'interrupted'
    assert scheduler.sessions.get('killed').status == 'interrupted'

    seen: dict = {}
    monkeypatch.setattr(pilot, '_agent', lambda _: Agent(FunctionModel(_model_that_reports_what_it_saw(seen)),
                                                         output_type=str))
    body, status = scheduler.pilot_message('killed', prompt)
    assert status == 200 and '核查现场' in body, body
    assert scheduler.sessions.get('killed').status == 'active'

    # The model sees what the dead process did and that the tool never returned a result.
    parts = [part for message in seen['messages'] for part in getattr(message, 'parts', [])]
    returns = {getattr(part, 'tool_call_id', None): part for part in parts
               if getattr(part, 'part_kind', '') == 'tool-return'}
    assert returns['call-read'].content == 'recorded-result'
    assert returns['call-read'].outcome == 'success'
    assert returns['call-slow'].outcome == 'interrupted'
    assert 'interrupted' in returns['call-slow'].content
    expected_prompts = ['先读一下现场'] + ([prompt] if prompt is not None else [])
    assert [part.content for part in parts if getattr(part, 'part_kind', '') == 'user-prompt'] == expected_prompts

    # The conversation the user reads keeps both user messages and the reply, despite the framework
    # merging the consecutive requests it was handed.
    messages = json.loads(scheduler.pilot_messages('killed')[0])['messages']
    assert [message['text'] for message in messages] == [*expected_prompts, '我看到了中断，先核查现场。']


def test_the_file_record_keeps_the_events_and_snapshots_the_killed_process_wrote(tmp_path):
    root, _config = _kill_a_pilot_process(tmp_path)
    runs = sorted((root / 'state' / 'pilot-steps').iterdir())
    assert len(runs) == 1, runs
    assert (runs[0] / 'run.json').is_file()
    assert (runs[0] / 'events.jsonl').is_file()
    assert (runs[0] / 'tool_effects.jsonl').is_file()
    assert list((runs[0] / 'snapshots').glob('*.json'))
    events = [json.loads(line) for line in (runs[0] / 'events.jsonl').read_text().splitlines()]
    assert [event['kind'] for event in events][-1] == 'tool_call_started'
    effects = [json.loads(line) for line in (runs[0] / 'tool_effects.jsonl').read_text().splitlines()]
    assert effects[-1]['status'] == 'started'


def test_a_long_pilot_history_is_compacted_for_the_request():
    """A long conversation reuses the harness compaction, and says so in the history it keeps."""
    from pydantic_ai.messages import ModelRequest, ModelResponse, TextPart, UserPromptPart
    from anchor.pilot import _compaction

    raw = {"models": [], "pilot_compaction": {"max_messages": 6, "keep_messages": 2}}
    sizes = []

    def model(messages, info):
        sizes.append(len(messages))
        return ModelResponse(parts=[TextPart(content=f"reply {len(sizes)}")])

    agent = Agent(FunctionModel(model), output_type=str,
                  capabilities=_compaction(raw, {"context_window": 0}))
    history = [ModelRequest(parts=[UserPromptPart(content="the first question")])]
    for index in range(5):
        history = asyncio.run(agent.run(f"question {index}", message_history=history)).all_messages()
    assert sizes[2] == 5, f"the conversation was not growing before the bound: {sizes}"
    assert max(sizes) <= 6, f"the model was sent {max(sizes)} messages, over the configured bound"
    assert len(history) <= 6, "the compacted history is what continues"
    # The receipt is the honest part: the model is told its memory before that point is secondhand.
    text = repr(history)
    assert "History before this point" in text, text[:400]
    # Compaction is opt-out, and a configuration without it builds no strategy.
    assert _compaction({"pilot_compaction": {"enabled": False}}, {}) == []
    assert _compaction({}, {})[0].__class__.__name__ == "SlidingWindowCompaction"
