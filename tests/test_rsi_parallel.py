"""Exercise the actual RSI graph, native parallel nodes and two distinct feedback paths."""
import json
from pathlib import Path
import shlex

from anchor.simple import run as runner

ROOT = Path(__file__).resolve().parents[1]


def test_rsi_same_run_specialists_reaudit_and_revise_then_publish(tmp_path):
    graph = json.loads((ROOT / 'examples/graphs/rsi.json').read_text())
    fixture = tmp_path / 'fixture'
    (fixture / 'domains').mkdir(parents=True)
    window = {'start': '2026-09-24T00:00:00Z', 'end_exclusive': '2026-10-01T00:00:00Z'}
    (fixture / 'index.json').write_text(json.dumps(window))
    (fixture / 'previous.json').write_text('[]')
    (fixture / 'previous-index.json').write_text('{"proposals":[]}')
    for domain in ('runs', 'code', 'graphs', 'plugins', 'dependencies'):
        (fixture / 'domains' / (domain + '.json')).write_text('{}')
    graph['ops']['collect']['run'] = 'mkdir -p evidence; cp -r /local-inputs/fixture/. evidence/'
    graph['ops']['research']['run'] = 'mkdir -p research; echo \'{"limitations":["fixture"]}\' > research/ecosystem.json; echo fixture > research/sources.md'
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    (workspace / 'graph.json').write_text(json.dumps(graph))
    (workspace / 'local-inputs.json').write_text(json.dumps({
        'collect': {'fixture': str(fixture)}, 'review': {'code': str(ROOT)}, 'gate': {'code': str(ROOT)}}))
    config = tmp_path / 'runtime.json'
    config.write_text('{"models":[]}')
    script = {}
    for node, domain in [('run-audit', 'runs'), ('code-audit', 'code'), ('graph-audit', 'graphs'),
                         ('plugin-audit', 'plugins'), ('dependency-audit', 'dependencies')]:
        findings = {'domain': domain, 'summary': 'fixture audit', 'coverage': {
            'read': [{'path': f'/in/collect/evidence/domains/{domain}.json', 'locator': '$'}],
            'not_reviewed': [], 'limitations': ['fixture only']}, 'findings': [], 'follow_up': []}
        check_feedback = """from pathlib import Path
p=Path('visits'); n=int(p.read_text())+1 if p.exists() else 1
feedback=Path('/in/audit-context/audit-feedback.txt').read_text()
assert ('FIRST_AUDIT' if n == 1 else 'REVISE') in feedback, feedback
if n == 2:
    assert 'inspect interface' in feedback, feedback
p.write_text(str(n))
"""
        script[node] = ['python3 -c ' + shlex.quote(check_feedback) + ' && printf %s ' +
                        shlex.quote(json.dumps(findings)) + ' > findings.json; echo audit > findings.md']
    evolution = {'window': window, 'proposals': [], 'carry_forward': []}
    script['analyze'] = ['printf %s ' + shlex.quote(json.dumps(evolution)) +
                          ' > evolution.json; echo report > rsi-report.md; echo sources > sources.md']
    review_code = '''import json, pathlib, subprocess
p=pathlib.Path('visits'); n=int(p.read_text())+1 if p.exists() else 1; p.write_text(str(n))
decision={1:'reaudit',2:'revise'}.get(n,'publish')
review={'decision':decision,'reviewed_commit':subprocess.check_output(['git','--git-dir=/in/analyze/.git','rev-parse','HEAD'],text=True).strip(),
'checks':dict.fromkeys(['facts','architecture','research','writing'],True),
'issues':[{'id':'I1','status':'open','domain':'code','required_change':'inspect interface'}] if n == 1 else []}
pathlib.Path('review.json').write_text(json.dumps(review)); pathlib.Path('review.md').write_text(decision)
'''
    script['fact-review'] = ['python3 -c ' + shlex.quote(review_code)]
    script['proposal-review'] = ['python3 -c ' + shlex.quote(review_code.replace(
        "decision={1:'reaudit',2:'revise'}.get(n,'publish')", "decision='publish'").replace(
        "if n == 1 else []", "if False else []"))]
    state = runner.run(workspace, config_path=config, model_script=script, run_id='parallel-rsi')
    assert state.status == 'finished', state.error
    assert state.runs['audit-fanout'] == state.runs['audit-join'] == 2
    assert state.runs['audit-context'] == 2
    assert state.runs['analyze'] == state.runs['review'] == state.runs['gate'] == 3
    assert state.runs['review-fanout'] == state.runs['review-join'] == 3
    assert state.runs['fact-review'] == state.runs['proposal-review'] == 3
    assert state.runs['collect'] == state.runs['publish'] == 1
    assert all(state.runs[node] == 2 for node in script if node.endswith('-audit'))
    for node in script:
        if node.endswith('-audit'):
            assert (workspace / 'runs/parallel-rsi' / node / 'visits').read_text() == '2'
    assert len(list((workspace / 'runs').iterdir())) == 1
    published = workspace / 'runs/parallel-rsi/publish'
    assert (published / 'gate.txt').read_text().startswith('PASS')
    assert json.loads((published / 'audit-manifest.json').read_text())['invocation'] == 2
    assert json.loads((published / 'review-manifest.json').read_text())['invocation'] == 3
    assert len(json.loads((published / 'review.json').read_text())['reviewers']) == 2
