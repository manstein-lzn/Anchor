import importlib.util
import json
from datetime import datetime
from pathlib import Path
import subprocess

import pytest

from anchor.simple.run import _local_inputs
from anchor.scheduling import next_after
from anchor.simple.graph import load

MODULE = Path(__file__).resolve().parents[1] / 'scripts/weekly_work_report/collect.py'
spec = importlib.util.spec_from_file_location('weekly_collect', MODULE)
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)


def test_history_window_generation_and_evidence(tmp_path):
    codex, deepseek = tmp_path / 'codex', tmp_path / 'dsh'
    codex.mkdir()
    deepseek.mkdir()
    end = datetime.fromisoformat('2026-10-01T09:00:00+08:00')
    rows = [
        {'type': 'session_meta', 'payload': {'cwd': '/work/project', 'id': 'c1'}},
        {'timestamp': '2026-09-24T01:00:00Z', 'type': 'response_item', 'payload': {
            'type': 'message', 'role': 'user', 'content': [{'text': '解决恢复问题'}]}},
        {'timestamp': '2026-10-01T01:00:00Z', 'type': 'response_item', 'payload': {
            'type': 'message', 'role': 'assistant', 'content': [{'text': '不在窗口'}]}},
        {'timestamp': '2026-09-25T01:00:00Z', 'type': 'response_item', 'payload': {
            'type': 'message', 'role': 'assistant', 'channel': 'analysis',
            'content': [{'text': '隐藏推理不进入报告'}]}},
        {'timestamp': '2026-09-25T01:00:00Z', 'type': 'response_item', 'payload': {
            'type': 'function_call_output', 'output': 'Authorization: Bearer abcdefghijklmnop'}}]
    (codex / 'session.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in rows))
    dsh = [{'type': 'session', 'cwd': '/work/project', 'id': 'd1'}, {
        'type': 'tool/result', 'time': 1790298000000,
        'data': {'message': {'content': [{'type': 'text', 'text': '2 passed [exit code: 0]'}]}}}]
    data = ''.join(json.dumps(r) + '\n' for r in dsh).encode()
    (deepseek / 'session.v4.jsonl.zstd').write_bytes(subprocess.run(
        ['zstd', '-c'], input=data, capture_output=True, check=True).stdout)
    (deepseek / 'session.v3.jsonl').write_bytes(data)
    index = collector.collect({'codex': codex, 'deepseek': deepseek}, tmp_path / 'out', end)
    assert len(index['sessions']) == 2
    assert index['sources']['codex']['events_in_window'] == 2
    assert index['sources']['deepseek']['events_in_window'] == 1
    text = '\n'.join(p.read_text() for p in (tmp_path / 'out').glob('*.jsonl'))
    assert '不在窗口' not in text and '隐藏推理' not in text
    assert 'abcdefghijklmnop' not in text and '[REDACTED]' in text
    assert '2 passed' in text and '"line": 2' in text
    graph = load(MODULE.parents[2] / 'examples/graphs/weekly-work-report.json')
    assert graph.routes('understand') == ('write',)
    assert graph.routes('write') == ('review',)
    assert graph.routes('review') == ('gate',)
    assert set(graph.routes('gate')) == {'write', 'understand', 'publish'}
    assert graph.routes('publish') == ('docmost',)
    assert graph.nodes['docmost'].plugins == ('docmost',)
    assert graph.agents['docmost_publisher'].network is True
    assert 'MsteinL' in graph.agents['docmost_publisher'].instructions
    assert 'Anchor周报' in graph.agents['docmost_publisher'].instructions
    assert '至少提供一幅自包含 SVG' in graph.agents['writer'].instructions
    assert '不能在正文重复标题' in graph.agents['writer'].instructions
    assert '少用“本周、这意味着' in graph.agents['writer'].instructions
    assert '优先使用主动语态和直接动词' in graph.agents['writer'].instructions
    assert '不强求章节等长' in graph.agents['writer'].instructions
    assert 'attachments_upload_page_image' in graph.agents['docmost_publisher'].instructions
    assert '删除正文第一行的 Markdown 一级标题' in graph.agents['docmost_publisher'].instructions
    assert next_after({'type': 'weekly', 'time': '09:00', 'weekdays': [3]},
                      datetime(2026, 9, 28, 20)) == datetime(2026, 10, 1, 9)


def test_local_inputs_require_operator_grant(tmp_path):
    history = tmp_path / 'history'
    history.mkdir()
    assert _local_inputs(tmp_path, ['collect']) == {}
    grants = tmp_path / 'local-inputs.json'
    grants.write_text(json.dumps({'collect': {'history': str(history)}}))
    assert _local_inputs(tmp_path, ['collect']) == {
        'collect': ((str(history), '/local-inputs/history'),)}
    grants.write_text(json.dumps({'collect': {'history': '/root'}}))
    with pytest.raises(ValueError, match='invalid tool mount'):
        _local_inputs(tmp_path, ['collect'])
    grants.write_text(json.dumps({'unknown': {'history': str(history)}}))
    with pytest.raises(ValueError, match='existing node'):
        _local_inputs(tmp_path, ['collect'])


def test_deepseek_thinking_uses_native_compatibility():
    from anchor.node.model_bridge import model_for
    from pydantic_ai.models import ModelRequestParameters
    from pydantic_ai.tools import ToolDefinition
    model = model_for({'model': 'deepseek-flash', 'base_url': 'https://example.invalid/v1'}, secret='test')
    _, choice = model._get_tool_choice({}, ModelRequestParameters(
        function_tools=[ToolDefinition(name='bash', parameters_json_schema={'type': 'object'})],
        allow_text_output=False))
    assert choice == 'auto'
    assert model.profile['openai_chat_send_back_thinking_parts'] == 'field'
    assert model.model_name == 'deepseek-flash'


def test_svg_download_can_render_as_image_without_active_content(tmp_path):
    from http.client import HTTPConnection
    from http.server import ThreadingHTTPServer
    import threading
    from anchor.serve import Handler
    svg = tmp_path / 'figure.svg'
    svg.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>')

    class Files(Handler):
        def do_GET(self):
            self._send_file(svg)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Files)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        connection = HTTPConnection(*server.server_address)
        connection.request('GET', '/figure.svg')
        response = connection.getresponse()
        assert response.status == 200
        assert response.getheader('Content-Type') == 'image/svg+xml'
        assert response.getheader('Content-Disposition').startswith('attachment;')
        assert "sandbox; default-src 'none'" in response.getheader('Content-Security-Policy')
        assert response.getheader('X-Content-Type-Options') == 'nosniff'
        assert response.read() == svg.read_bytes()
        connection.close()
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


def test_graph_op_reads_only_its_granted_source(tmp_path):
    from anchor.simple.run import run
    history = tmp_path / 'history'
    history.mkdir()
    (history / 'work.txt').write_text('verified work\n')
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    graph = {'entry': 'collect', 'ops': {
        'collect': {'writes': ['evidence.txt'], 'run':
            'cat /local-inputs/history/work.txt > evidence.txt && '
            '! touch /local-inputs/history/forbidden'},
        'publish': {'reads': ['evidence.txt'], 'writes': ['report.md'], 'run':
            'test ! -e /local-inputs/history/work.txt && cat /in/collect/evidence.txt > report.md'}},
        'nodes': [{'id': 'collect', 'op': 'collect'}, {'id': 'publish', 'op': 'publish'}],
        'edges': [{'from': 'collect', 'to': 'publish'}]}
    (workspace / 'graph.json').write_text(json.dumps(graph))
    (workspace / 'local-inputs.json').write_text(json.dumps({'collect': {'history': str(history)}}))
    config = tmp_path / 'runtime.json'
    config.write_text('{}')
    state = run(workspace, config_path=config, run_id='proof')
    assert state.status == 'finished'
    assert (workspace / 'runs/proof/publish/report.md').read_text() == 'verified work\n'
    assert not (history / 'forbidden').exists()


def _review(decision='publish'):
    return {'decision': decision, 'reviewed_commit': 'current', 'summary': 'Checked the draft against evidence',
            'checks': dict.fromkeys(['facts', 'business', 'reasoning', 'writing'], True), 'issues': []}


def _issue(identifier='R1', status='open'):
    return {'id': identifier, 'status': status, 'location': 'Summary paragraph',
            'evidence': 'session.jsonl:2 shows only a local test',
            'impact': 'Overstates customer readiness', 'required_change': 'Qualify the claim',
            'acceptance': 'Report distinguishes local tests from user acceptance',
            'resolution': 'Summary now says local tests only' if status != 'open' else ''}


@pytest.mark.parametrize('problem', ['open_issue', 'failed_check', 'stale_draft', 'lost_issue', 'empty_feedback'])
def test_review_gate_rejects_false_approvals(problem):
    gate_spec = importlib.util.spec_from_file_location('weekly_gate', MODULE.with_name('review_gate.py'))
    gate = importlib.util.module_from_spec(gate_spec)
    gate_spec.loader.exec_module(gate)
    review, previous = _review(), {}
    if problem == 'open_issue':
        review['issues'] = [_issue()]
    elif problem == 'failed_check':
        review['checks']['facts'] = False
    elif problem == 'stale_draft':
        review['reviewed_commit'] = 'previous'
    elif problem == 'lost_issue':
        previous['issues'] = [_issue()]
    else:
        review['issues'] = [_issue(status='resolved')]
        review['issues'][0]['acceptance'] = ''
    with pytest.raises(ValueError):
        gate.validate(review, previous, 'current')
    approved = _review()
    approved['issues'] = [_issue(status='limited')]
    assert gate.validate(approved, {'issues': [_issue()]}, 'current') == 'publish'


def test_weekly_feedback_revises_with_latest_inputs_before_publication(tmp_path):
    from anchor.simple.run import run
    import shlex

    graph = json.loads((MODULE.parents[2] / 'examples/graphs/weekly-work-report.json').read_text())
    # Synthetic evidence, real gate/Op/sandbox/Git/routing; only model answers are scripted.
    graph['ops']['collect']['run'] = (
        "mkdir -p evidence && printf '{}' > evidence/index.json && "
        "cp /local-inputs/code/review_gate.py review_gate.py")
    # This feedback-loop test exercises local routing; the external Docmost side effect is covered by the graph contract assertions above.
    graph['nodes'] = [node for node in graph['nodes'] if node['id'] != 'docmost']
    graph['edges'] = [edge for edge in graph['edges'] if edge['from'] != 'publish']
    workspace = tmp_path / 'feedback'
    workspace.mkdir()
    (workspace / 'graph.json').write_text(json.dumps(graph))
    (workspace / 'local-inputs.json').write_text(json.dumps({'collect': {'code': str(MODULE.parent)}}))
    config = tmp_path / 'runtime.json'
    config.write_text('{}')

    analyst = """
from pathlib import Path
import json
version = 1
if Path('analysis.md').exists():
    feedback = json.loads(Path('/in/gate/review.json').read_text())
    assert feedback['decision'] == 'understand'
    assert feedback['issues'][-1]['id'] == 'R2'
    version = 2
Path('analysis.md').write_text(str(version))
Path('sources.md').write_text(f'Evidence revision {version}')
"""
    writer = """
from pathlib import Path
import json
version = int(Path('version').read_text()) + 1 if Path('version').exists() else 1
if version == 2:
    assert json.loads(Path('/in/gate/review.json').read_text())['issues'][0]['id'] == 'R1'
if version == 3:
    assert Path('/in/understand/analysis.md').read_text() == '2'
Path('version').write_text(str(version))
Path('report.md').write_text(f'# Draft {version}\\n![Evidence boundary](assets/figure.svg)\\n')
Path('sources.md').write_text(Path('/in/understand/sources.md').read_text())
Path('assets').mkdir(exist_ok=True)
Path('assets/figure.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>')
"""
    reviewer = """
from pathlib import Path
import json, subprocess
previous = json.loads(Path('review.json').read_text()) if Path('review.json').exists() else {}
number = previous.get('round', 0) + 1
review = REVIEW
review['round'] = number
review['reviewed_commit'] = subprocess.check_output(['git','--git-dir=/in/write/.git','rev-parse','HEAD'],text=True).strip()
assert Path('/in/write/report.md').read_text().startswith(f'# Draft {number}')
assert Path('/in/collect/evidence/index.json').exists()
review['issues'] = [ISSUE]
if number == 1:
    review['decision'] = 'write'
    review['checks']['writing'] = False
elif number == 2:
    review['issues'][0]['status'] = 'resolved'
    review['issues'][0]['resolution'] = 'Draft 2 corrected the explanation'
    review['issues'].append(dict(ISSUE, id='R2'))
    review['decision'] = 'understand'
    review['checks']['facts'] = False
else:
    review['issues'] = previous['issues']
    for issue in review['issues']:
        issue['status'] = 'resolved'
        issue['resolution'] = 'Corrected against Evidence revision 2'
    assert Path('/in/write/sources.md').read_text() == 'Evidence revision 2'
Path('review.json').write_text(json.dumps(review))
Path('review.md').write_text('Specific review with issue IDs R1 and R2 and source locations.')
""".replace('REVIEW', repr(_review())).replace('ISSUE', repr(_issue()))

    def command(code):
        return 'python -c ' + shlex.quote(code)

    state = run(workspace, config_path=config, run_id='review-loop', model_script={
        'understand': [command(analyst)], 'write': [command(writer)], 'review': [command(reviewer)]})
    out = workspace / 'runs/review-loop'
    assert state.status == 'finished', state.error
    assert state.passes['collect'] == 1  # feedback must not move the reporting time window
    assert state.passes['write'] == 3
    assert state.passes['understand'] == 2
    assert (out / 'publish/report.md').read_text().startswith('# Draft 3')
    assert (out / 'publish/sources.md').read_text() == 'Evidence revision 2'
    published_review = json.loads((out / 'publish/review.json').read_text())
    assert [issue['id'] for issue in published_review['issues']] == ['R1', 'R2']
    assert all(issue['status'] == 'resolved' for issue in published_review['issues'])
