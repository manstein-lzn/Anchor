import json

import pytest

from scripts.rsi import gate


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value))


@pytest.fixture
def evidence(tmp_path, monkeypatch):
    monkeypatch.setattr(gate, '_commit', lambda directory: directory.name + '-commit')
    ref = {'path': '/in/collect/evidence/index.json', 'locator': 'start'}
    window = {'start': '2026-09-24T00:00:00Z', 'end_exclusive': '2026-10-01T00:00:00Z'}
    write(tmp_path / 'collect/evidence/index.json', window)
    write(tmp_path / 'collect/evidence/previous.json', [{'ledger': {'proposals': [{'id': 'RSI-old'}]}}])
    branches = []
    for domain in sorted(gate.DOMAINS):
        node = domain + '-audit'
        write(tmp_path / node / 'findings.json', {
            'domain': domain, 'summary': 'reviewed',
            'coverage': {'read': [ref], 'not_reviewed': [], 'limitations': []},
            'findings': [{'id': domain + '-1', 'claim': 'observed', 'kind': 'observed',
                          'confidence': 'high', 'evidence': [ref]}]})
        branches.append({'output': node, 'nodes': [{'node': node, 'commit': node + '-commit'}]})
    write(tmp_path / 'audit-join/join.json', {'branches': branches})
    write(tmp_path / 'analyze/evolution.json', {'window': window, 'proposals': [],
                                              'carry_forward': [{'id': 'RSI-old', 'status': 'hold',
                                                                 'reason': 'need validation'}]})
    (tmp_path / 'analyze/rsi-report.md').write_text('# Report')
    (tmp_path / 'analyze/sources.md').write_text('source')
    write(tmp_path / 'review/review.json', {'decision': 'publish', 'reviewed_commit': 'analyze-commit',
        'checks': dict.fromkeys(['facts', 'architecture', 'research', 'writing'], True), 'issues': []})
    return tmp_path


def test_gate_publishes_only_after_current_specialists_and_proposal_continuity(evidence):
    assert gate.evaluate(evidence)[0] == 'publish'


def test_missing_or_stale_specialist_returns_to_fanout(evidence):
    (evidence / 'runs-audit/findings.json').unlink()
    assert gate.evaluate(evidence)[0] == 'audit-context'


def test_join_commit_mismatch_is_not_fixed_by_rewriting_report(evidence, monkeypatch):
    monkeypatch.setattr(gate, '_commit', lambda directory: 'different')
    target, reasons = gate.evaluate(evidence)
    assert target == 'audit-context' and any('joined commit' in reason for reason in reasons)


def test_silently_dropped_prior_proposal_cannot_publish(evidence):
    path = evidence / 'analyze/evolution.json'
    value = json.loads(path.read_text())
    value['carry_forward'] = []
    write(path, value)
    target, reasons = gate.evaluate(evidence)
    assert target == 'analyze' and any('RSI-old' in reason for reason in reasons)


def test_resolved_proposal_requires_readable_current_evidence(evidence):
    path = evidence / 'analyze/evolution.json'
    value = json.loads(path.read_text())
    value['carry_forward'][0]['status'] = 'resolved'
    write(path, value)
    assert gate.evaluate(evidence)[0] == 'analyze'


def test_stale_review_and_malformed_evolution_do_not_publish(evidence):
    review = json.loads((evidence / 'review/review.json').read_text())
    review['reviewed_commit'] = 'old'
    write(evidence / 'review/review.json', review)
    assert gate.evaluate(evidence)[0] == 'analyze'
    (evidence / 'analyze/evolution.json').write_text('not-json')
    assert gate.evaluate(evidence)[0] == 'analyze'


def test_review_can_request_new_specialist_evidence(evidence):
    review = json.loads((evidence / 'review/review.json').read_text())
    review['decision'] = 'reaudit'
    review['issues'] = [{'status': 'open', 'domain': 'code', 'required_change': 'inspect interface'}]
    write(evidence / 'review/review.json', review)
    assert gate.evaluate(evidence)[0] == 'audit-context'


def test_evidence_cannot_escape_input_snapshots(evidence):
    ref = {'path': '/in/../outside', 'locator': '1'}
    assert not gate._references([ref], evidence)
    assert gate._references([{'path': '/in/collect/evidence/*.json', 'locator': 'all listed artifacts'}], evidence)
    assert not gate._references([{'path': '/in/collect/absent/*.json', 'locator': 'missing'}], evidence)
    directory = [{'path': '/in/collect/evidence', 'locator': 'directory scope'}]
    assert gate._references(directory, evidence, coverage=True)
    assert not gate._references(directory, evidence)


def test_window_and_coverage_are_mandatory(evidence):
    path = evidence / 'code-audit/findings.json'
    value = json.loads(path.read_text())
    value['coverage']['read'] = []
    write(path, value)
    assert gate.evaluate(evidence)[0] == 'audit-context'


def test_conflicting_duplicate_proposal_ids_cannot_publish(evidence):
    path = evidence / 'analyze/evolution.json'
    value = json.loads(path.read_text())
    value['carry_forward'].append({'id': 'RSI-old', 'status': 'continue', 'reason': 'conflicting'})
    write(path, value)
    target, reasons = gate.evaluate(evidence)
    assert target == 'analyze' and any('duplicate ID' in reason for reason in reasons)


def test_invalid_specialist_reference_feedback_identifies_path(evidence):
    path = evidence / 'plugins-audit/findings.json'
    value = json.loads(path.read_text())
    bad_path = '/in/collect/evidence/{a,b}/**'
    value['findings'][0]['evidence'] = [{'path': bad_path, 'locator': 'files'}]
    write(path, value)
    target, reasons = gate.evaluate(evidence)
    assert target == 'audit-context'
    assert any(bad_path in reason and 'one reference per file' in reason for reason in reasons)
