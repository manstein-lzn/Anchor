"""Check current specialist artifacts, proposal continuity and commit-bound review before publish."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess

try:
    from scripts.rsi.commit_binding import verify_commit
except ModuleNotFoundError:  # Executed directly inside the sandbox input mount.
    from commit_binding import verify_commit


DOMAINS = {'runs', 'code', 'graphs', 'plugins', 'dependencies'}


def _commit(directory: Path) -> str:
    return subprocess.check_output(['git', f'--git-dir={directory / ".git"}', 'rev-parse', 'HEAD'],
                                   text=True, timeout=10).strip()


def _object(path: Path) -> dict:
    value = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(value, dict):
        raise ValueError(f'{path.name} must be an object')
    return value


def _references(refs, inputs: Path, *, coverage: bool = False) -> bool:
    if not isinstance(refs, list) or not refs:
        return False
    for ref in refs:
        if not isinstance(ref, dict) or not isinstance(ref.get('path'), str) or not ref.get('locator'):
            return False
        path = ref['path']
        if not path.startswith('/in/'):
            return False
        relative = path[len('/in/'):]
        if '..' in Path(relative).parts or Path(relative).is_absolute():
            return False
        candidates = list(inputs.glob(relative)) if any(char in relative for char in '*?[') else [inputs / relative]
        if not candidates or any(not candidate.resolve().is_relative_to(inputs.resolve())
                                 or not (candidate.is_file() or coverage and candidate.is_dir())
                                 for candidate in candidates):
            return False
    return True


def _audit_errors(inputs: Path) -> list[str]:  # noqa: C901 - finite artifact contract
    errors = []
    manifest = _object(inputs / 'audit-join/join.json')
    branches = manifest.get('branches')
    if not isinstance(branches, list) or len(branches) != len(DOMAINS):
        return ['audit-join must contain all five specialist branches']
    domains = set()
    for branch in branches:
        if not isinstance(branch, dict) or not isinstance(branch.get('output'), str):
            errors.append('malformed join branch')
            continue
        name = branch['output']
        if '/' in name or name in ('', '.', '..'):
            errors.append('invalid specialist node reference')
            continue
        try:
            directory = inputs / name
            record = next(item for item in branch['nodes'] if item['node'] == name)
            try:
                if isinstance(record['commit'], str):
                    if record['commit'] != _commit(directory):
                        raise ValueError('joined commit does not match the branch Git HEAD')
                else:
                    verify_commit(directory, record['commit'], node=name)
            except (OSError, ValueError, subprocess.SubprocessError) as exc:
                errors.append(f'{name}: findings do not belong to the joined commit ({exc})')
            data = _object(directory / 'findings.json')
            domain = data.get('domain')
            if not isinstance(domain, str) or domain not in DOMAINS or domain in domains:
                errors.append(f'{name}: unknown or duplicate domain')
            else:
                domains.add(domain)
            if not isinstance(data.get('summary'), str) or not data['summary'].strip():
                errors.append(f'{name}: missing summary')
            coverage = data.get('coverage', {})
            if (not isinstance(coverage, dict) or not _references(coverage.get('read'), inputs, coverage=True)
                    or not isinstance(coverage.get('not_reviewed'), list)
                    or not isinstance(coverage.get('limitations'), list)):
                errors.append(f'{name}: coverage must identify read evidence and unreviewed/limited scope')
            findings = data.get('findings')
            if not isinstance(findings, list):
                errors.append(f'{name}: findings must be a list')
                continue
            ids = set()
            for finding in findings:
                if (not isinstance(finding, dict) or not isinstance(finding.get('id'), str)
                        or not finding.get('id') or finding['id'] in ids):
                    errors.append(f'{name}: finding needs a unique ID')
                    continue
                ids.add(finding['id'])
                if (finding.get('kind') not in ('observed', 'inferred', 'unknown')
                        or finding.get('confidence') not in ('high', 'medium', 'low')
                        or not finding.get('claim')):
                    errors.append(f'{name}/{finding["id"]}: invalid claim kind or confidence')
                if not _references(finding.get('evidence'), inputs):
                    errors.append(f'{name}/{finding["id"]}: unreadable evidence {finding.get("evidence")!r}; '
                                  'use existing /in/ file paths with locators, one reference per file; '
                                  'shell brace expressions and directories are not evidence files')
        except (OSError, ValueError, TypeError, KeyError, StopIteration, subprocess.SubprocessError) as exc:
            errors.append(f'{name}: invalid specialist artifact ({type(exc).__name__})')
    if domains != DOMAINS:
        errors.append(f'missing specialist domains: {sorted(DOMAINS - domains)}')
    return errors


def _previous_ids(value) -> set[str]:
    """Prior collector versions may nest reports/ledgers differently; preserve every proposal ID."""
    ids = set()
    if isinstance(value, dict):
        for key, child in value.items():
            if key in ('proposals', 'carry_forward') and isinstance(child, list):
                ids.update(item['id'] for item in child if isinstance(item, dict) and isinstance(item.get('id'), str))
            ids.update(_previous_ids(child))
    elif isinstance(value, list):
        for child in value:
            ids.update(_previous_ids(child))
    return ids


def _proposal_errors(inputs: Path) -> list[str]:
    evolution = _object(inputs / 'analyze/evolution.json')
    index = _object(inputs / 'collect/evidence/index.json')
    previous = json.loads((inputs / 'collect/evidence/previous.json').read_text())
    errors = []
    if evolution.get('window') != {key: index[key] for key in ('start', 'end_exclusive')}:
        errors.append('evolution.window must equal the collected evidence window')
    proposals, carry = evolution.get('proposals'), evolution.get('carry_forward')
    if not isinstance(proposals, list) or not isinstance(carry, list):
        return errors + ['proposals and carry_forward must be lists']
    seen = set()
    for item in [*proposals, *carry]:
        if not isinstance(item, dict) or not isinstance(item.get('id'), str) or not item['id']:
            errors.append('proposal continuity entry needs an ID')
            continue
        if item['id'] in seen:
            errors.append(f'{item["id"]}: duplicate ID across proposals/carry_forward')
        seen.add(item['id'])
        if item.get('status') not in ('proposed', 'continue', 'hold', 'resolved'):
            errors.append(f'{item["id"]}: invalid status')
        if item in proposals or item.get('status') == 'resolved':
            if not _references(item.get('evidence'), inputs):
                errors.append(f'{item["id"]}: missing readable evidence')
        if item in proposals and any(not item.get(key) for key in ('scope', 'change', 'acceptance', 'risk', 'rollback')):
            errors.append(f'{item["id"]}: missing change/acceptance/risk/rollback')
        if item in carry and not item.get('reason'):
            errors.append(f'{item["id"]}: missing continuity reason')
        if item in carry and item.get('status') == 'proposed':
            errors.append(f'{item["id"]}: carry_forward must use continue/hold/resolved')
    if missing := _previous_ids(previous) - seen:
        errors.append(f'previous proposal IDs disappeared: {sorted(missing)}')
    return errors


def evaluate(inputs: Path) -> tuple[str, list[str]]:
    try:
        audit_errors = _audit_errors(inputs)
    except (OSError, ValueError, TypeError, KeyError) as exc:
        audit_errors = [f'cannot verify joined audits: {type(exc).__name__}: {exc}']
    if audit_errors:
        return 'audit-context', audit_errors
    reasons = [f'missing {name}' for name in ('rsi-report.md', 'evolution.json', 'sources.md')
               if not (inputs / 'analyze' / name).is_file()]
    try:
        reasons.extend(_proposal_errors(inputs))
        review = _object(inputs / 'review/review.json')
        if review.get('reviewed_commit') != _commit(inputs / 'analyze'):
            reasons.append('review does not correspond to the current analysis commit')
        elif review.get('decision') == 'reaudit':
            return 'audit-context', ['review requests specialist re-audit', json.dumps(review.get('issues', []), ensure_ascii=False)]
        if review.get('decision') != 'publish':
            reasons.append(f'review decision is {review.get("decision")!r}')
        checks = review.get('checks')
        if not isinstance(checks, dict) or any(checks.get(key) is not True
                                              for key in ('facts', 'architecture', 'research', 'writing')):
            reasons.append('review checks are incomplete')
        issues = review.get('issues')
        if not isinstance(issues, list) or any(not isinstance(item, dict) or item.get('status') not in ('resolved', 'limited')
                                              for item in issues):
            reasons.append('review has open or malformed issues')
        if review.get('decision') != 'publish':
            reasons.append(json.dumps(review.get('issues', []), ensure_ascii=False))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        reasons.append(f'cannot verify publication artifacts: {type(exc).__name__}: {exc}')
    return ('analyze', reasons) if reasons else ('publish', ['review, branch commits and proposal continuity verified'])


def main() -> None:
    target, reasons = evaluate(Path('/in'))
    Path('gate.txt').write_text(('PASS' if target == 'publish' else 'REVISE') + '\n' +
                                f'target={target}\n' + '\n'.join(f'- {reason}' for reason in reasons) + '\n',
                                encoding='utf-8')
    subprocess.run(['anchor-route', '--to', target, '--reason', '; '.join(reasons)[:500]], check=True)


if __name__ == '__main__':
    main()
