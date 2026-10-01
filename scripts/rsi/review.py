"""Merge independent fact and proposal reviews without allowing one to waive the other."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess


REVIEWERS = {'fact-review': ('facts', 'research'), 'proposal-review': ('architecture', 'writing')}
UNSAFE_ROLLBACK_PHRASES = (
    '全量暴露', '整文件不脱敏', '关闭脱敏', '取消权限保护', '绕过权限',
)
OVERSTRONG_FACT_PHRASES = (
    '只能来自未提交工作树或服务重启',
    '本次审计 run 是首次 fanout/join 执行',
    '本次审计run是首次fanout/join执行',
    '才是首次 fanout/join',
    '才是首次fanout/join',
    '零 production completed 样本',
    '零production completed样本',
)


def _commit(path: Path) -> str:
    return subprocess.check_output(['git', f'--git-dir={path / ".git"}', 'rev-parse', 'HEAD'],
                                   text=True, timeout=10).strip()


def aggregate(inputs: Path) -> dict:  # noqa: C901 - two independent artifact contracts
    commit = _commit(inputs / 'analyze')
    checks = dict.fromkeys(('facts', 'research', 'architecture', 'writing'), False)
    issues, records, summaries, errors, decisions = [], [], [], [], []
    try:
        evolution_path = inputs / 'analyze/evolution.json'
        report_path = inputs / 'analyze/rsi-report.md'
        evolution = json.loads(evolution_path.read_text()) if evolution_path.exists() else {}
        report = report_path.read_text(encoding='utf-8') if report_path.exists() else ''
        proposal_text = json.dumps(evolution, ensure_ascii=False) + '\n' + report
        for phrase in UNSAFE_ROLLBACK_PHRASES:
            if phrase in proposal_text:
                issues.append({'id': f'safety-rollback-{phrase}', 'status': 'open',
                               'domain': 'architecture', 'location': 'evolution.json/rsi-report.md',
                               'evidence': f'unsafe rollback phrase: {phrase}',
                               'required_change': '保留脱敏、权限和证据保护；隔离受影响材料或退回安全版本，不得用扩大暴露换取可用性',
                               'acceptance': '提案及报告不再建议关闭保护或回退到更宽权限'})
        for phrase in OVERSTRONG_FACT_PHRASES:
            if phrase in proposal_text:
                issues.append({'id': f'fact-scope-{phrase[:12]}', 'status': 'open',
                               'domain': 'facts', 'location': 'evolution.json/rsi-report.md',
                               'evidence': f'overstrong unbounded claim: {phrase}',
                               'required_change': '限定为本采集窗口/当前部署定义/隔离验收 Run；不要把 invocation=1 或时间相关性当作历史首次或完备因果',
                               'acceptance': '报告区分生产 Run、隔离验收 Run 与本次证据包，并保留未排除的构建/版本可能性'})
    except (OSError, ValueError, TypeError) as exc:
        errors.append(f'cannot inspect proposal safety: {type(exc).__name__}: {exc}')
    try:
        branches = json.loads((inputs / 'review-join/join.json').read_text())['branches']
        if not isinstance(branches, list) or len(branches) != len(REVIEWERS):
            raise ValueError('review-join requires both independent reviewers')
        outputs = [branch['output'] for branch in branches]
        if sorted(outputs) != sorted(REVIEWERS):
            raise ValueError('review-join must bind fact-review and proposal-review exactly once')
        for branch in branches:
            name = branch['output']
            directory = inputs / name
            joined = next(item['commit'] for item in branch['nodes'] if item['node'] == name)
            if joined != _commit(directory):
                raise ValueError(f'{name}: review is not the joined commit')
            data = json.loads((directory / 'review.json').read_text())
            if not isinstance(data, dict):
                raise ValueError(f'{name}: review must be an object')
            if data.get('reviewed_commit') != commit:
                raise ValueError(f'{name}: reviewed_commit differs from current analysis')
            decision = data.get('decision')
            if decision not in ('publish', 'revise', 'reaudit'):
                raise ValueError(f'{name}: invalid decision')
            decisions.append(decision)
            owned_checks = data.get('checks')
            if not isinstance(owned_checks, dict):
                raise ValueError(f'{name}: checks must be an object')
            for key in REVIEWERS[name]:
                checks[key] = owned_checks.get(key) is True
            local_issues = data.get('issues')
            if not isinstance(local_issues, list):
                raise ValueError(f'{name}: issues must be a list')
            seen = set()
            for issue in local_issues:
                if (not isinstance(issue, dict) or not isinstance(issue.get('id'), str)
                        or not issue['id'] or issue['id'] in seen
                        or issue.get('status') not in ('open', 'resolved', 'limited')):
                    raise ValueError(f'{name}: malformed or duplicate issue')
                seen.add(issue['id'])
                issues.append({**issue, 'id': f'{name}/{issue["id"]}', 'reviewer': name})
            records.append({'node': name, 'commit': joined, 'reviewed_commit': commit,
                            'decision': decision})
            summaries.append(f'{name}: {data.get("summary", "")}')
    except (OSError, ValueError, TypeError, KeyError, StopIteration, subprocess.SubprocessError) as exc:
        errors.append(str(exc))
    if errors:
        issues.append({'id': 'review-contract', 'status': 'open', 'domain': 'review',
                       'required_change': '; '.join(errors)})
    if errors:
        decision = 'revise'
    elif 'reaudit' in decisions:
        decision = 'reaudit'
    elif ('revise' in decisions or not all(checks.values())
          or any(issue['status'] == 'open' for issue in issues)):
        decision = 'revise'
    else:
        decision = 'publish'
    return {'decision': decision, 'reviewed_commit': commit, 'checks': checks, 'issues': issues,
            'reviewers': records, 'summary': '\n'.join(summaries + errors)}


def main() -> None:
    data = aggregate(Path('/in'))
    Path('review.json').write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
    Path('review.md').write_text(data['decision'] + '\n\n' + data['summary'] + '\n')


if __name__ == '__main__':
    main()
