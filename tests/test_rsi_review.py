import json

import pytest

from scripts.rsi import review


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data))


@pytest.fixture
def reviews(tmp_path, monkeypatch):
    monkeypatch.setattr(review, '_commit', lambda path: path.name + '-commit')
    branches = []
    for name, checks in review.REVIEWERS.items():
        write(tmp_path / name / 'review.json', {
            'decision': 'publish', 'reviewed_commit': 'analyze-commit',
            'checks': dict.fromkeys(checks, True), 'issues': [], 'summary': name})
        branches.append({'output': name, 'nodes': [{'node': name, 'commit': name + '-commit'}]})
    write(tmp_path / 'review-join/join.json', {'branches': branches})
    return tmp_path


def test_independent_reviewers_must_both_approve_current_analysis(reviews):
    result = review.aggregate(reviews)
    assert result['decision'] == 'publish'
    assert all(result['checks'].values())
    assert len(result['reviewers']) == 2


@pytest.mark.parametrize('node', review.REVIEWERS)
def test_one_publish_cannot_override_other_open_issue(reviews, node):
    path = reviews / node / 'review.json'
    data = json.loads(path.read_text())
    data['issues'] = [{'id': 'unsafe-rollback', 'status': 'open',
                       'required_change': 'retain redaction on failure'}]
    write(path, data)
    result = review.aggregate(reviews)
    assert result['decision'] == 'revise'
    assert result['issues'][0]['id'] == node + '/unsafe-rollback'


def test_reaudit_preserves_domain_and_feedback(reviews):
    path = reviews / 'fact-review/review.json'
    data = json.loads(path.read_text())
    data.update(decision='reaudit', issues=[{'id': 'missing', 'status': 'open',
                                           'domain': 'plugins', 'required_change': 'check binding'}])
    write(path, data)
    result = review.aggregate(reviews)
    assert result['decision'] == 'reaudit'
    assert result['issues'][0]['domain'] == 'plugins'


def test_unsafe_rollback_cannot_publish_even_if_both_reviewers_approve(reviews):
    write(reviews / 'analyze/evolution.json', {'proposals': [{'rollback': '回退到当前全量暴露'}]})
    (reviews / 'analyze/rsi-report.md').write_text('若失败则回退到当前全量暴露')
    result = review.aggregate(reviews)
    assert result['decision'] == 'revise'
    assert any(issue['id'] == 'safety-rollback-全量暴露' for issue in result['issues'])


@pytest.mark.parametrize('phrase', review.OVERSTRONG_FACT_PHRASES)
def test_unbounded_historical_or_causal_claim_cannot_publish(reviews, phrase):
    write(reviews / 'analyze/evolution.json', {'decision': phrase})
    (reviews / 'analyze/rsi-report.md').write_text(phrase)
    result = review.aggregate(reviews)
    assert result['decision'] == 'revise'


@pytest.mark.parametrize('damage', ['stale_analysis', 'stale_join', 'missing_reviewer',
                                   'malformed_review', 'missing_check', 'duplicate_issue'])
def test_incomplete_or_stale_reviews_cannot_publish(reviews, damage):
    path = reviews / 'proposal-review/review.json'
    data = json.loads(path.read_text())
    if damage == 'stale_analysis':
        data['reviewed_commit'] = 'old'
    elif damage in ('stale_join', 'missing_reviewer'):
        manifest = reviews / 'review-join/join.json'
        value = json.loads(manifest.read_text())
        if damage == 'stale_join':
            value['branches'][0]['nodes'][0]['commit'] = 'old'
        else:
            value['branches'].pop()
        write(manifest, value)
    elif damage == 'malformed_review':
        data = []
    elif damage == 'missing_check':
        data['checks'].pop('architecture')
    else:
        data['issues'] = [{'id': 'duplicate', 'status': 'resolved'}] * 2
    write(path, data)
    assert review.aggregate(reviews)['decision'] == 'revise'
