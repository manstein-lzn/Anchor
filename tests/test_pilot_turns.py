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
        # A stopped turn is not silently replaced; the user resumes it explicitly.
        assert call('POST', '/sessions/stop-turn/turns', {'request_id': 'r2', 'message': '新问题'})[0] == 409
        status, data = call('POST', '/sessions/stop-turn/turns', {'request_id': 'r3', 'resume': True})
        assert status == 202, data
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and scheduler.turns.get('stop-turn', data['turn']['id'])['status'] == 'running':
            time.sleep(0.05)
        assert scheduler.turns.get('stop-turn', data['turn']['id'])['status'] == 'completed'
        assert json.loads(scheduler.pilot_messages('stop-turn')[0])['messages'][-1]['text'] == '继续完成'
    finally:
        release.set()
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()


def test_turn_pauses_for_approval_then_resumes_the_same_tool_call(tmp_path, monkeypatch):
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
                            {'request_id': 'r1', 'message': '启动 demo'})
        assert status == 202, data
        paused = settle(data['turn']['id'])
        assert paused['status'] == 'waiting_approval'
        assert started == [], 'the Run must not start before the user approves'
        session = scheduler.sessions.get('approve')
        assert session.status == 'waiting_user'
        pending, = session.approvals
        assert (pending['action'], pending['target']) == ('graph_run', 'demo')
        assert pending['proposal'] == {'graph': 'demo'}
        status, refused = call('POST', '/sessions/approve/turns',
                               {'request_id': 'r-other', 'message': '顺便做点别的'})
        assert status == 409 and 'pending operation' in refused['error']

        status, _ = call('POST', '/sessions/approve/confirm',
                         {'action': 'graph_run', 'approval_key': pending['key']})
        assert status == 200
        status, data = call('POST', '/sessions/approve/turns', {'request_id': 'r2', 'resume': True})
        assert status == 202, data
        settled = settle(data['turn']['id'])
        assert settled['status'] == 'completed'
        assert started == ['demo']
        assert scheduler.sessions.get('approve').run_ids == ['run-1']
        assert scheduler.sessions.get('approve').approvals == []
        assert json.loads(scheduler.pilot_messages('approve')[0])['messages'][-1]['text'] == '已启动 demo'
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


def test_a_graph_edited_while_the_user_decides_is_not_overwritten(tmp_path, monkeypatch):
    workspace = tmp_path / 'workspaces' / 'demo'
    workspace.mkdir(parents=True)
    graph = workspace / 'graph.json'
    graph.write_text(json.dumps({'nodes': ['original']}), encoding='utf-8')
    calls = []

    async def stream(messages, info):
        calls.append(messages)
        if len(calls) == 1:
            yield {0: DeltaToolCall(
                name='graph_update',
                json_args=json.dumps({'graph': 'demo', 'definition': {'nodes': ['from model']}}),
                tool_call_id='edit-1')}
        else:
            yield '没有覆盖你的修改'

    def agent(_):
        built = Agent(FunctionModel(stream_function=stream), output_type=[str, DeferredToolRequests])
        _register_tools(built)
        return built

    monkeypatch.setattr(pilot, '_agent', agent)
    scheduler = Scheduler(tmp_path, tmp_path / 'config.json')
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
                            {'request_id': 'r1', 'message': '改一下 demo'})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'waiting_approval'
        pending, = scheduler.sessions.get('edit').approvals
        assert pending['precondition']['graph'] == 'demo'

        # The user edits the same Graph in the canvas while the confirmation is still open.
        graph.write_text(json.dumps({'nodes': ['edited by the user']}), encoding='utf-8')
        status, _ = call('POST', '/sessions/edit/confirm',
                         {'action': 'graph_update', 'approval_key': pending['key']})
        assert status == 200
        status, data = call('POST', '/sessions/edit/turns', {'request_id': 'r2', 'resume': True})
        assert status == 202, data
        assert settle(data['turn']['id'])['status'] == 'completed'
        assert json.loads(graph.read_text(encoding='utf-8'))['nodes'] == ['edited by the user']
        assert json.loads(scheduler.pilot_messages('edit')[0])['messages'][-1]['text'] == '没有覆盖你的修改'
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join()
