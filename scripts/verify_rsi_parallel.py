"""Run the full RSI on authorized local evidence with the configured real model, in isolation."""
import argparse
from datetime import datetime
import json
from pathlib import Path
from tempfile import mkdtemp
import time
import signal

from anchor.runtime.secrets import load_dotenv
from anchor.serve import Scheduler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--anchor', type=Path, default=Path('.local/demo'))
    parser.add_argument('--source', type=Path, default=Path.cwd())
    parser.add_argument('--config', type=Path, default=Path('.local/runtime.json'))
    parser.add_argument('--analysis-model', help='Configured model alias for synthesis and both reviews')
    args = parser.parse_args()
    load_dotenv()
    root = Path(mkdtemp(prefix='rsi-parallel-', dir='.local')).resolve()
    source = args.source.resolve()
    scheduler = Scheduler(root, args.config.resolve())
    graph = json.loads((source / 'examples/graphs/rsi.json').read_text())
    if args.analysis_model:
        for role in ('analyst', 'fact-review', 'proposal-review'):
            graph['agents'][role]['model'] = args.analysis_model
    payload, code = scheduler.create('rsi', graph)
    assert code == 200, payload
    grants = root / 'workspaces/rsi/local-inputs.json'
    grants.write_text(json.dumps({'collect': {'source': str(source), 'anchor': str(args.anchor.resolve()),
                                            'code': str(source), 'grants': str(grants)},
                                 'research': {'code': str(source)}, 'review': {'code': str(source)},
                                 'gate': {'code': str(source)}}))
    payload, code = scheduler.trigger('rsi', None)
    assert code == 202, payload
    run = json.loads(payload)['run']
    def request_stop(signum, frame):
        scheduler.control_run(run, 'stop')
        print('Acceptance stop requested; waiting for active nodes to settle', flush=True)
    signal.signal(signal.SIGINT, request_stop)
    signal.signal(signal.SIGTERM, request_stop)
    print(json.dumps({'root': str(root), 'run': run}), flush=True)
    samples = []
    last = None
    while True:
        detail = scheduler.run('rsi', run)
        if detail:
            state = detail['state']
            snapshot = {'status': state['status'], 'active': list(state.get('active', {})),
                        'cursor': state.get('cursor'), 'executed': state['executed']}
            if snapshot != last:
                samples.append({'at': datetime.now().isoformat(), **snapshot})
                (root / 'samples.json').write_text(json.dumps(samples, ensure_ascii=False, indent=2))
                print(json.dumps(snapshot, ensure_ascii=False), flush=True)
                last = snapshot
            if state['status'] != 'running':
                break
        time.sleep(1)
    evidence = {'run': run, 'status': state['status'], 'error': state.get('error'),
                'content_acceptance': 'requires independent source-bound review',
                'observed_max_active': max(len(item['active']) for item in samples),
                'executions': state['runs'], 'single_run': len(list((root / 'workspaces/rsi/runs').iterdir())) == 1}
    (root / 'evidence.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2))
    assert state['status'] == 'finished', evidence
    assert evidence['observed_max_active'] >= 2, evidence
    print(json.dumps({'workflow_passed': True, 'evidence': str(root / 'evidence.json')}), flush=True)


if __name__ == '__main__':
    main()
