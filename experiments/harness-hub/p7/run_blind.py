"""Prepare and run exactly one isolated, real-provider P7 trial.

The defaults reproduce the blind trial: group A data, Host source at 1af26b1,
binaries in ROOT/../bin. --data, --source and --bin point a later round at
other data, the source that produced it, and the Host built from that source.
No grading answers are given to the Run. `prepare` never contacts a model and
records its choices in source-manifest.json; `run` reads them from there and
refuses an already-started directory.
"""

import argparse
import io
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tarfile
import time

os.umask(0o077)
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
SOURCE = '1af26b1273acba4fc422e0e2723798dd0d87afa5'
sys.path.insert(0, str(HERE.parent / 'p0'))
import run_p0  # noqa: E402
sys.path.insert(0, str(HERE / 'graph'))
import build  # noqa: E402


def save(path, value):
    run_p0.save(path, value)


def fingerprint(root):
    return {str(p.relative_to(root)): run_p0.sha(p)
            for p in sorted(root.rglob('*')) if p.is_file() and not p.is_symlink()}


def prepare(root, data, source, bindir, run_id, guard):
    root.mkdir(parents=True, exist_ok=False)
    for directory in ('home', 'library/plugins/harness-hub', 'operator/rsi-signals',
                      'catalog', 'bundle', 'host-source', 'anchor-rsi-source', 'hub-source'):
        (root / directory).mkdir(parents=True)
    archive = subprocess.check_output(['git', 'archive', source, 'rust'], cwd=REPO)
    # This trusted commit archive contains source only: no .git, local state,
    # experiments or task documents. Later commits cannot be read with git show.
    with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
        stream.extractall(root / 'host-source', filter='data')
    shutil.copytree(REPO / 'rust/anchor-rsi/src', root / 'anchor-rsi-source/src')
    shutil.copytree(REPO / 'rust/anchor-hub/src', root / 'hub-source/src')
    graph = build.graph()
    save(root / 'bundle/graph.json', graph)
    save(root / 'bundle/manifest.json',
         {'format': 1, 'graph': 'graph.json', 'plugins': [{'id': 'harness-hub'}]})
    shutil.copyfile(REPO / 'examples/rust-rsi/plugins/harness-hub/plugin.json',
                    root / 'library/plugins/harness-hub/plugin.json')
    binary = bindir / 'anchor-hub'
    assert binary.is_file(), 'build and copy anchor-hub first'
    for name in ('anchor-runner-host', 'anchor-net-relay'):
        assert (bindir / name).is_file(), f'{bindir / name} is missing'
    bundled_binary = root / 'library/plugins/harness-hub/bin/anchor-hub'
    bundled_binary.parent.mkdir()
    shutil.copyfile(binary, bundled_binary)
    bundled_binary.chmod(0o700)
    subprocess.run(['strip', str(bundled_binary)], check=True)
    # Which node gets which grant is the Graph's (graph/grants.json, written
    # by graph/build.py); where each grant points is this launcher's.
    sources = {'harness-hub-data': data, 'hub-bin': bundled_binary.parent,
               'host-source': root / 'host-source', 'hub-source': root / 'hub-source',
               'anchor-rsi-source': root / 'anchor-rsi-source'}
    needed = json.loads((HERE / 'graph/grants.json').read_text())
    grants = {node: {name: str(sources[name]) for name in names}
              for node, names in needed.items()}
    save(root / 'operator/rsi-signals/local-inputs.json', grants)
    save(root / 'source-manifest.json', {
        'host_source_commit': source,
        'data': str(data), 'bin': str(bindir), 'run_id': run_id, 'guard_seconds': guard,
        'host_source_files': fingerprint(root / 'host-source'),
        'hub_source_files': fingerprint(root / 'hub-source'),
        'anchor_rsi_source_files': fingerprint(root / 'anchor-rsi-source'),
        'data_files_before': fingerprint(data),
        'graph_sha256': run_p0.sha(root / 'bundle/graph.json'),
        'plugin_sha256': run_p0.sha(root / 'library/plugins/harness-hub/plugin.json'),
        'excluded': ['.git', 'experiments', 'docs', '.local', 'other data roots'],
    })
    # No model is called by this sandboxed MCP preflight.
    command = ['bwrap', '--die-with-parent', '--unshare-all', '--ro-bind', '/usr', '/usr',
               '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib64', '/lib64',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp',
               '--ro-bind', str(bundled_binary.parent.parent), '/plugins/harness-hub',
               '--ro-bind', str(data), '/local-inputs/harness-hub-data',
               '/plugins/harness-hub/bin/anchor-hub', 'mcp', '/local-inputs/harness-hub-data']
    messages = [
        {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize',
         'params': {'protocolVersion': '2025-06-18'}},
        {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call',
         'params': {'name': 'signals', 'arguments': {}}},
    ]
    done = subprocess.run(command, input='\n'.join(map(json.dumps, messages)) + '\n',
                          text=True, capture_output=True, check=True)
    replies = [json.loads(line) for line in done.stdout.splitlines()]
    table = json.loads(replies[1]['result']['content'][0]['text'])
    assert not replies[1]['result']['isError'] and table['signals']
    save(root / 'mcp-preflight.json', {'replies': replies, 'stderr': done.stderr})
    print(json.dumps({'prepared': str(root), 'signals': len(table['signals'])}))


def run(root):
    # Creating this marker is the one-run gate, before any provider contact.
    marker = root / 'started.json'
    with marker.open('x') as stream:
        json.dump({'started_unix': time.time()}, stream)
    before = json.loads((root / 'source-manifest.json').read_text())
    data, bindir = Path(before['data']), Path(before['bin'])
    run_id, timeout = before['run_id'], before['guard_seconds']
    pid, _, live = run_p0.live_host()
    host = bindir / 'anchor-runner-host'
    env = {k: v for k, v in live.items()
           if k in run_p0.FORWARD or k in run_p0.RUNTIME
           or k.startswith(('ANCHOR_GOOSE_', 'ANCHOR_MODEL_'))}
    env.update({
        'HOME': str(root / 'home'),
        'ANCHOR_RUNNER_STATE_ROOT': str(root / 'state'),
        'ANCHOR_RUNNER_WORKSPACE_ROOT': str(root / 'work'),
        'ANCHOR_RUNNER_BUNDLE_ROOT': str(root / 'bundle'),
        'ANCHOR_RUNNER_CATALOG_ROOT': str(root / 'catalog'),
        'ANCHOR_RUNNER_LIBRARY_ROOT': str(root / 'library'),
        'ANCHOR_RUNNER_LOCAL_INPUTS_ROOT': str(root / 'operator'),
        'ANCHOR_RUNNER_GRAPH_NAME': 'rsi-signals',
        'ANCHOR_RUNNER_ALLOWED_COMMANDS': 'sh,cat,printf,grep,test,true,ls,mkdir,cp,find,sed,head,wc,python3,python3.12',
        'ANCHOR_GOOSE_RELAY_BINARY': str(bindir / 'anchor-net-relay'),
    })
    model = root / 'state/managed/model-config.json'
    model.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(REPO / '.local/rust/state/managed/model-config.json', model)
    model.chmod(0o600)
    request = json.dumps({'op': 'start_bundle', 'version': 1, 'request_id': run_id,
                          'run_id': run_id, 'input': {}}).encode()
    save(root / 'identity.json', {
        'live_environment_pid': pid, 'host_source_commit': before['host_source_commit'],
        'host_sha256': run_p0.sha(host),
        'hub_sha256': run_p0.sha(root / 'library/plugins/harness-hub/bin/anchor-hub'),
        'goose_sha256': run_p0.sha(env['ANCHOR_GOOSE_BINARY']),
        'relay_sha256': run_p0.sha(bindir / 'anchor-net-relay'),
        'model_alias': 'models.default.flash', 'guard_seconds': timeout,
    })
    started = time.time()
    guard = False
    try:
        process = subprocess.Popen([str(host)], cwd=root / 'home', env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        save(root / 'process.json', {'pid': process.pid, 'run': run_id})
        try:
            stdout, stderr = process.communicate(struct.pack('>I', len(request)) + request,
                                                 timeout=timeout)
        except subprocess.TimeoutExpired:
            guard = True
            os.killpg(process.pid, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=30)
    finally:
        model.unlink(missing_ok=True)
    (root / 'stdout.bin').write_bytes(stdout)
    (root / 'stderr.log').write_bytes(stderr)
    response = None
    if len(stdout) >= 4:
        size = struct.unpack('>I', stdout[:4])[0]
        response = json.loads(stdout[4:4 + size])
    save(root / 'response.json', response)
    save(root / 'meta.json', {
        'task': 'P7', 'index': 1, 'run': run_id, 'root': str(root),
        'started_unix': started, 'ended_unix': time.time(),
        'wall_seconds': time.time() - started, 'returncode': process.returncode, 'guard': guard,
        'data_unchanged': fingerprint(data) == before['data_files_before'],
        'host_source_unchanged': fingerprint(root / 'host-source') == before['host_source_files'],
        'model_snapshot_removed': not model.exists(),
    })
    print(json.dumps(json.loads((root / 'meta.json').read_text())))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'run'))
    parser.add_argument('--root', type=Path, default=REPO / '.local/harness-hub-p7/blind-01')
    parser.add_argument('--data', type=Path, default=REPO / '.local/harness-hub-p6/exp1/A')
    parser.add_argument('--source', default=SOURCE, help='commit of the Host that made --data')
    parser.add_argument('--bin', type=Path, help='anchor-hub, the Host built from --source '
                        'and its relay; default ROOT/../bin')
    parser.add_argument('--run-id', default='p7-blind-01')
    parser.add_argument('--guard', type=int, default=1800, help='seconds before the Host is stopped')
    args = parser.parse_args()
    root = args.root.resolve()
    if args.action == 'prepare':
        prepare(root, args.data.resolve(), args.source,
                (args.bin or root.parent / 'bin').resolve(), args.run_id, args.guard)
    else:
        run(root)
