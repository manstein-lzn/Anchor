"""Explicit real-provider same-Run parallel acceptance, in an isolated data root.

Run: ./.venv/bin/python scripts/verify_parallel_provider.py
Uses the existing .env model profile. No Plugin or external business operation is invoked.
Evidence includes sampled Run states, native traces, commits and the actual synthesis artifact.
"""
import argparse
import json
import time
from pathlib import Path
from tempfile import mkdtemp

from anchor.runtime.secrets import load_dotenv
from anchor.serve import Scheduler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=Path('.local/runtime.json'))
    args = parser.parse_args()
    load_dotenv()
    root = Path(mkdtemp(prefix='parallel-provider-', dir='.local')).resolve()
    graph = json.loads(Path('examples/graphs/parallel-audit.json').read_text())
    graph['objective'] = '并行验证：两个分支各写一份固定标记文件，综合节点读取真实文件。'
    graph['nodes'][1]['with'] = '用 Bash 将 ARCHITECTURE-OK 写入 findings.md；然后返回简短的完成结果。'
    graph['nodes'][2]['with'] = '用 Bash 将 RECOVERY-OK 写入 findings.md；然后返回简短的完成结果。'
    graph['agents']['synthesizer']['instructions'] = (
        '用 Bash 读取 /in/join/join.json、/in/architecture/findings.md 和 /in/recovery/findings.md。'
        '将两份 findings.md 原文合并写入 report.md，必须包含两条标记。完成后返回简短的结构化结果。')
    scheduler = Scheduler(root, args.config.resolve())
    payload, code = scheduler.create('parallel-check', graph)
    assert code in (200, 201), payload
    payload, code = scheduler.trigger('parallel-check', None)
    assert code == 202, payload
    run_id = json.loads(payload)['run']
    print(f'Evidence: {root}; Run: {run_id}', flush=True)
    samples = []
    # An acceptance observer deadline, not a Graph completion budget.
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        detail = scheduler.run('parallel-check', run_id)
        if detail is not None:
            state = detail['state']
            samples.append({'at': time.time(), 'status': state['status'],
                            'active': list(state.get('active', {})), 'cursor': state.get('cursor'),
                            'executed': state['executed']})
            if state['status'] != 'running':
                break
        time.sleep(0.05)
    else:
        scheduler.control_run(run_id, 'stop')
        raise RuntimeError('acceptance observer timed out; requested Run stop')
    (root / 'samples.json').write_text(json.dumps(samples, indent=2))
    assert state['status'] == 'finished', state.get('error')
    assert any(set(item['active']) == {'architecture', 'recovery'} for item in samples)
    workspace = root / 'workspaces/parallel-check'
    assert len(list((workspace / 'runs').iterdir())) == 1
    directory = workspace / 'runs' / run_id
    report = (directory / 'synthesize/report.md').read_text()
    assert 'ARCHITECTURE-OK' in report and 'RECOVERY-OK' in report, report
    joined = json.loads((directory / 'join/join.json').read_text())
    for branch in joined['branches']:
        for result in branch['nodes']:
            assert state['nodes'][result['node']]['commit'] == result['commit']
    evidence = {'status': 'passed', 'run': run_id, 'overlap_observed': True,
                'single_run': True, 'join': joined, 'report': report}
    (root / 'evidence.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2))
    print(json.dumps({'status': 'passed', 'evidence': str(root / 'evidence.json')}), flush=True)


if __name__ == '__main__':
    main()
