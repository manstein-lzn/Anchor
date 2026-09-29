"""Validate a report review and route through Anchor's existing Op protocol."""
from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess

CHECKS = {'facts', 'business', 'reasoning', 'writing'}
DECISIONS = {'publish', 'write', 'understand'}


def validate(review: dict, previous: dict, commit: str) -> str:  # noqa: C901 - one flat review contract
    if (not isinstance(review, dict) or not isinstance(review.get('decision'), str)
            or review['decision'] not in DECISIONS):
        raise ValueError('review must name publish, write or understand')
    if review.get('reviewed_commit') != commit:
        raise ValueError('review does not refer to the current manuscript commit')
    if not isinstance(review.get('summary'), str) or not review['summary'].strip():
        raise ValueError('review needs a reason for its decision')
    checks = review.get('checks')
    if not isinstance(checks, dict) or set(checks) != CHECKS or any(type(v) is not bool for v in checks.values()):
        raise ValueError('review needs four boolean checks: facts, business, reasoning, writing')
    issues = review.get('issues')
    if not isinstance(issues, list):
        raise ValueError('review issues must be a list')
    ids = set()
    for issue in issues:
        if not isinstance(issue, dict):
            raise ValueError('each issue must be an object')
        for field in ('id', 'location', 'evidence', 'impact', 'required_change', 'acceptance'):
            if not isinstance(issue.get(field), str) or not issue[field].strip():
                raise ValueError(f'each issue needs a concrete {field}')
        if issue['id'] in ids or issue.get('status') not in {'open', 'resolved', 'limited'}:
            raise ValueError('issue IDs must be unique and status must be open, resolved or limited')
        ids.add(issue['id'])
        if issue['status'] != 'open' and (not isinstance(issue.get('resolution'), str)
                                        or not issue['resolution'].strip()):
            raise ValueError('closed issues need evidence of resolution or an acceptable claim limitation')
    if not {i['id'] for i in previous.get('issues', [])} <= ids:
        raise ValueError('previous issues must be carried forward and explicitly resolved')
    unresolved = any(i['status'] == 'open' for i in issues)
    decision = review['decision']
    if decision == 'publish' and (unresolved or not all(checks.values())):
        raise ValueError('publication requires all four checks and no unresolved blocking issue')
    if decision != 'publish' and (not unresolved or all(checks.values())):
        raise ValueError('return decisions need an open issue and a failed check')
    if decision == 'write' and not checks['facts']:
        raise ValueError('unresolved factual evidence belongs to understand, not a writing rewrite')
    return decision


def main() -> None:
    incoming = Path('/in/review')
    review = json.loads((incoming / 'review.json').read_text())
    previous_path = Path('review.json')
    previous = json.loads(previous_path.read_text()) if previous_path.exists() else {}
    commit = subprocess.check_output(['git', '--git-dir=/in/write/.git', 'rev-parse', 'HEAD'], text=True).strip()
    decision = validate(review, previous, commit)
    if not (incoming / 'review.md').read_text().strip():
        raise ValueError('human-readable review is missing')
    for name in ('review.json', 'review.md'):
        shutil.copy2(incoming / name, name)
    subprocess.run(['anchor-route', '--to', decision, '--reason', review['summary']], check=True)


if __name__ == '__main__':
    main()
