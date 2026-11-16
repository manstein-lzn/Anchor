"""Replay saved publication inputs through the real Host, without a model."""

import argparse
import copy
import json
from pathlib import Path
import struct
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
sys.path.insert(0, str(HERE.parent / 'p0'))
import run_p0  # noqa: E402
sys.path.insert(0, str(HERE / 'graph'))
import build  # noqa: E402


def replay(collected, root):
    # A distinct directory and an Op-only Graph keep this verification separate
    # from the one blind provider trial. Never edit or resume its history.
    root.mkdir(parents=True, exist_ok=False)
    for directory in ('bundle', 'home', 'catalog', 'library', 'inputs',
                      'operator/rsi-signals-gate-replay'):
        (root / directory).mkdir(parents=True)
    graph = build.graph()
    for node, name in (('synthesize', 'proposals.json'), ('synthesize', 'synthesis.md'),
                       ('review', 'review.md')):
        source = collected / 'artifacts' / node / '1' / name
        (root / 'inputs' / name).write_bytes(source.read_bytes())
    ops = {node: copy.deepcopy(graph['ops'][node]) for node in ('gate', 'publish')}
    for node, name in (('synthesize', 'proposals.json'), ('review', 'review.md')):
        ops[node] = {'run': f'cp /local-inputs/trial/{name} {name}', 'writes': [name]}
    ops['synthesize']['run'] += '; cp /local-inputs/trial/synthesis.md synthesis.md'
    ops['synthesize']['writes'].append('synthesis.md')
    # A revision routes back into synthesize. Its second pass stops on purpose
    # instead of retrying, so the fixture never stands in for a model revision.
    ops['synthesize']['run'] = (
        "if test -e /in/gate/gate.txt; then printf '%s\\n' "
        "'Recorded review requires revision; no model retry.' >&2; exit 1; fi; "
        + ops['synthesize']['run'])
    # These two author declarations are unused by the publication command and
    # belong to the omitted research nodes, so they are absent in this fixture.
    ops['publish']['reads'] = [name for name in ops['publish']['reads']
                              if name not in ('signals.json', 'triage.md')]
    replay_graph = {
        'entry': 'synthesize', 'objective': 'Replay saved publication inputs without a model.',
        'ops': ops, 'nodes': [{'id': node, 'op': node} for node in ops],
        'edges': [{'from': a, 'to': b} for a, b in (
            ('synthesize', 'review'), ('review', 'gate'), ('gate', 'publish'),
            ('gate', 'synthesize'))],
    }
    run_p0.save(root / 'bundle/graph.json', replay_graph)
    run_p0.save(root / 'bundle/manifest.json', {'format': 1, 'graph': 'graph.json', 'plugins': []})
    run_p0.save(root / 'operator/rsi-signals-gate-replay/local-inputs.json', {
        node: {'trial': str(root / 'inputs')} for node in ('synthesize', 'review')})
    _, _, live = run_p0.live_host()
    env = {key: value for key, value in live.items() if key in run_p0.FORWARD}
    env.update({
        'HOME': str(root / 'home'),
        'ANCHOR_RUNNER_STATE_ROOT': str(root / 'state'),
        'ANCHOR_RUNNER_WORKSPACE_ROOT': str(root / 'work'),
        'ANCHOR_RUNNER_BUNDLE_ROOT': str(root / 'bundle'),
        'ANCHOR_RUNNER_CATALOG_ROOT': str(root / 'catalog'),
        'ANCHOR_RUNNER_LIBRARY_ROOT': str(root / 'library'),
        'ANCHOR_RUNNER_LOCAL_INPUTS_ROOT': str(root / 'operator'),
        'ANCHOR_RUNNER_GRAPH_NAME': 'rsi-signals-gate-replay',
        'ANCHOR_RUNNER_ALLOWED_COMMANDS': 'sh,cp,grep,cat,printf,python3,python3.12',
    })
    request = json.dumps({'op': 'start_bundle', 'version': 1,
                          'request_id': 'p7-gate-replay', 'run_id': 'p7-gate-replay',
                          'input': {}}).encode()
    host = root.parent / 'bin/anchor-runner-host'
    started = time.time()
    done = subprocess.run([str(host)], input=struct.pack('>I', len(request)) + request,
                          cwd=root / 'home', env=env, capture_output=True, timeout=120)
    (root / 'stdout.bin').write_bytes(done.stdout)
    (root / 'stderr.log').write_bytes(done.stderr)
    size = struct.unpack('>I', done.stdout[:4])[0]
    response = json.loads(done.stdout[4:4 + size])
    run_p0.save(root / 'response.json', response)
    run_p0.save(root / 'meta.json', {
        'verification': 'Op-only replay of recorded blind-trial proposals and review',
        'wall_seconds': time.time() - started, 'returncode': done.returncode,
        'host_sha256': run_p0.sha(host), 'model_calls': 0,
        'inputs': {name: run_p0.sha(root / 'inputs' / name)
                   for name in ('proposals.json', 'review.md')},
    })
    assert not list((root / 'state/goose-acp').glob('*.evidence.json'))
    assert response['kind'] == 'run', response
    artifacts = [json.loads(path.read_text())
                 for path in (root / 'state/artifacts').glob('*/manifest.json')]
    gate = next(item for item in artifacts if item['key']['node_id'] == 'gate')
    decision = (root / 'inputs/review.md').read_text().splitlines()[0]
    route = 'publish' if decision == 'PASS' else 'synthesize'
    assert gate['completion']['route'] == route, gate['completion']
    assert gate['completion']['output']['exit_code'] == 0
    if decision != 'PASS':
        assert 'Recorded review requires revision' in response['projection']['reason']
    else:
        assert response['projection']['outcome'] == 'done', response
    result = {'review': decision, 'gate_exit_code': 0, 'gate_route': route,
              'model_calls': 0, 'projection': response['projection']}
    run_p0.save(root / 'replay-result.json', result)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('collected', type=Path)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    replay(args.collected.resolve(), args.root.resolve())
