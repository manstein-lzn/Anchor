"""Opt-in real-provider HTTP/SSE acceptance; retains isolated evidence under .local/.

Run from the repository root: ./.venv/bin/python scripts/verify_pilot_provider.py
Uses the configured Pilot model and secret; never patches the model or tools.

What this covers, with the real provider:

* streaming text over the turn API, with a repeated submission returning the same turn;
* a Graph the user asked for is created without a second approval, and its Run starts from the same
  conversation, in the sandbox, with a real artifact;
* `session_ask` pauses the run and the answer arrives as that call's tool result;
* deleting a Graph still waits for the user, a refusal leaves the Graph alone, and a confirmation for
  a Graph that changed while the user was deciding is refused;
* a reply links the Graph and the Run it mentions in the form the workspace can open;
* the harness file record exists for every turn, which is what the kill acceptance reads.
"""

import argparse
import asyncio
import json
import time
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
from pathlib import Path
from tempfile import mkdtemp
from threading import Thread
from uuid import uuid4

from anchor.serve import Handler, Scheduler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=Path('.local/runtime.json'))
    args = parser.parse_args()
    Path('.local').mkdir(exist_ok=True)
    root = Path(mkdtemp(prefix='pilot-provider-', dir='.local')).resolve()
    scheduler = Scheduler(root, args.config.resolve())
    handler = type('ProviderHandler', (Handler,), {'scheduler': scheduler})
    server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    print(f'Evidence: {root}', flush=True)

    def request(method, path, body=None, expected=200):
        client = HTTPConnection(*server.server_address, timeout=180)
        try:
            client.request(method, path, json.dumps(body) if body is not None else None,
                           {'Content-Type': 'application/json'})
            response = client.getresponse()
            value = json.loads(response.read())
            assert response.status == expected, (path, response.status, value)
            return value
        finally:
            client.close()

    def session(name):
        return request('GET', f'/sessions/{name}')['session']

    def turn(name, message=None, expected='completed'):
        body = {'request_id': str(uuid4()),
                **({'message': message} if message is not None else {'resume': True})}
        path = f'/sessions/{name}/turns'
        accepted = request('POST', path, body, 202)['turn']
        assert request('POST', path, body, 202)['turn']['id'] == accepted['id']
        client = HTTPConnection(*server.server_address, timeout=180)
        try:
            client.request('GET', f'{path}/{accepted["id"]}/events')
            response = client.getresponse()
            assert response.status == 200
            wire = response.read().decode()
        finally:
            client.close()
        chunks = [json.loads(line[6:]) for line in wire.splitlines() if line.startswith('data: ')]
        terminal = next(item for item in chunks if item.get('id') == accepted['id'] and 'status' in item)
        assert terminal['status'] == expected, terminal
        assert request('POST', path, body, 202)['turn']['id'] == accepted['id']
        print(f'PASS {name}: {expected}, {len(chunks)} SSE records', flush=True)
        return {'chunks': chunks, 'turn': accepted, 'wire': wire}

    def reply(name):
        messages = request('GET', f'/sessions/{name}/messages')['messages']
        return messages[-1]['text'] if messages and messages[-1]['role'] == 'assistant' else ''

    def decide(name, action, approve=True):
        pending = session(name)['approvals']
        assert len(pending) == 1 and pending[0]['action'] == action, pending
        item = pending[0]
        request('POST', f'/sessions/{name}/{"confirm" if approve else "reject"}',
                {'action': action, 'approval_key': item['tool_call_id']})
        return item['tool_call_id']

    definition = {
        'entry': 'write', 'objective': 'Pilot provider acceptance',
        'ops': {'write': {'run': "printf 'provider-ok\\n' > result.txt", 'writes': ['result.txt']}},
        'nodes': [{'id': 'write', 'op': 'write'}], 'edges': [],
    }
    try:
        request('POST', '/sessions', {'id': 'work'}, 201)
        turn('work', '请直接调用 graph_create，name 为 provider-check，definition 为下面的完整 JSON。'
                     '只创建，不启动；工具返回后用一句话报告结果，不要重复调用。\n' + json.dumps(definition))
        assert request('GET', '/graphs/provider-check')['definition'] == definition

        turn('work', '现在请调用 graph_run 启动 provider-check，只启动一次；工具返回后简短报告结果。')
        linked = session('work')
        assert len(linked['run_ids']) == 1, linked
        run_id = linked['run_ids'][0]
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            run = request('GET', f'/runs/{run_id}')
            if run['state']['status'] != 'running':
                break
            time.sleep(0.1)
        assert run['state']['status'] == 'finished', run
        assert (root / 'workspaces/provider-check/runs' / run_id
                / 'write/result.txt').read_text() == 'provider-ok\n'
        assert len(list((root / 'workspaces/provider-check/runs').glob('*'))) == 1
        print(f'PASS graph_run: one sandbox run and artifact, {run_id}', flush=True)

        # The identifiers a reply names must be the ones the workspace can open.
        turn('work', f'用一句话说明运行 {run_id} 的结果，并在句子里用 Markdown 链接引用这个 Run'
                     f'（#anchor/run/{run_id}）和 Graph（#anchor/graph/provider-check）。')
        text = reply('work')
        assert f'#anchor/run/{run_id}' in text, text
        assert '#anchor/graph/provider-check' in text, text
        print('PASS links: the reply carries the Run and Graph references the workspace opens', flush=True)

        request('POST', '/sessions', {'id': 'reject'}, 201)
        turn('reject', '这是我授权的删除：请直接调用 graph_delete 删除 provider-check，不要先用 session_ask '
                       '问我；如果确认步骤被拒绝，只报告取消，不再尝试。', 'waiting_approval')
        decide('reject', 'graph_delete', approve=False)
        turn('reject')
        assert request('GET', '/graphs/provider-check')['definition'] == definition

        request('POST', '/sessions', {'id': 'stale'}, 201)
        turn('stale', '请直接调用 graph_delete 删除 provider-check，不要先用 session_ask 问我；'
                      '如果工具拒绝，只报告原因，不重新提案。', 'waiting_approval')
        edited = {**definition, 'objective': 'user edit while awaiting approval'}
        request('PUT', '/graphs/provider-check', {'definition': edited})
        decide('stale', 'graph_delete')
        chunks = turn('stale')['chunks']
        assert any(item.get('type') == 'tool-output-available' and item.get('output', {}).get('changed')
                   for item in chunks), chunks
        assert request('GET', '/graphs/provider-check')['definition'] == edited, \
            'the edited Graph must survive'

        request('POST', '/sessions', {'id': 'ask'}, 201)
        turn('ask', '请调用 session_ask 问我“验收口令是什么？”，收到回答后原样复述口令，不做其他操作。',
             'waiting_user')
        question = session('ask')['questions'][0]
        answer = 'anchor-provider-verified'
        turn('ask', answer)
        assert answer in reply('ask'), reply('ask')
        saved = asyncio.run(scheduler.sessions.conversation_store().get(
            conversation_id=session('ask')['conversation_id']))
        parts = [part for message in saved.messages for part in message.parts]
        assert any(getattr(p, 'tool_call_id', None) == question['tool_call_id']
                   and getattr(p, 'content', None) == answer and p.part_kind == 'tool-return'
                   for p in parts)
        assert sum(p.part_kind == 'user-prompt' for p in parts) == 1
        assert session('ask')['status'] == 'active'

        # The work record is the harness file store, and it is what a restarted process reads.
        records = sorted((root / 'state' / 'pilot-steps').iterdir())
        assert records, 'the harness file record is empty'
        for record in records:
            assert (record / 'events.jsonl').is_file(), record
            assert (record / 'run.json').is_file(), record
        print('PASS: direct graph work, deletion approval, refusal, stale resource, deferred answer, '
              'request deduplication, harness file record', flush=True)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == '__main__':
    main()
