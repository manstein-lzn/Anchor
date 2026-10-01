"""Local fanout/join coordination inside the ordinary Graph Run.

Workers only execute prepared nodes. The calling scheduler owns every state change,
input snapshot, commit and recovery decision; no child Run or separate store is created.
"""

from __future__ import annotations

import json
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from threading import Event


def _region(graph, state, regions):
    group = state.parallel
    if group is None or group.get('fanout') not in regions:
        raise RuntimeError('missing parallel activation')
    region = regions[group['fanout']]
    if group.get('join') != region.join or group.get('invocation') != state.runs.get(region.fanout):
        raise RuntimeError('parallel activation does not match its fanout invocation')
    members = {node for branch in region.branches for node in branch}
    completed = group['completed']
    if set(completed) - members or set(state.active) - members or set(completed) & set(state.active):
        raise RuntimeError('parallel activation has inconsistent members')
    for branch in region.branches:
        pending = False
        for node in branch:
            if node not in completed:
                pending = True
                continue
            if pending:
                raise RuntimeError('parallel branch completed out of dependency order')
            result = state.pass_result(node, completed[node])
            if not result.submitted or state.result(node).commit != result.commit:
                raise RuntimeError('parallel activation references an unsuccessful or stale result')
    return region


def control_result(graph, state, step, regions):
    """Produce a deterministic control-node artifact, then use the ordinary commit path."""
    from anchor.simple import run as runner

    if step.node_id in regions:
        region = regions[step.node_id]
        if state.parallel is not None:
            raise RuntimeError('fanout cannot overlap an active region')
        payload = {'fanout': region.fanout, 'join': region.join,
                   'invocation': state.runs[step.node_id],
                   'branches': [list(branch) for branch in region.branches]}
        name = 'fanout.json'
    else:
        region = _region(graph, state, regions)
        group = state.parallel
        if (step.node_id != region.join or state.active or group.get('failure')
                or any(node not in group['completed'] for branch in region.branches for node in branch)):
            raise RuntimeError('join requires every branch of this activation to succeed')
        branches = []
        for branch in region.branches:
            results = [state.pass_result(node, group['completed'][node]) for node in branch]
            branches.append({'entry': branch[0], 'output': branch[-1], 'status': 'completed',
                             'nodes': [{'node': result.node_id, 'commit': result.commit,
                                        'files': list(result.files), 'summary': result.submission}
                                       for result in results]})
        payload = {'fanout': region.fanout, 'join': region.join,
                   'invocation': group['invocation'], 'branches': branches}
        name = 'join.json'
    path = step.directory / name
    if path.is_symlink():
        raise ValueError(f'control output must not be a symlink: {name}')
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    return runner._result_of(step.node_id, '', step.directory, step.number,
                             {'submission': json.dumps(payload, ensure_ascii=False),
                              'exit_status': 'Submitted'}, step.inputs)


def run_branches(graph, state, decided, run_dir, regions, prepare, settle, stop_request,  # noqa: C901
                 preserve_partial):
    """Drive one active region until join is ready, or pause/stop/failure has drained workers."""
    from anchor.simple import run as runner

    region = _region(graph, state, regions)
    group = state.parallel
    if group.get('failure'):
        state.status, state.error = 'failed', group['failure']
        state.save(run_dir)
        return False
    cancel = Event()
    futures = {}
    error = None

    def asked():
        return stop_request() if stop_request is not None else None

    def cancelled():
        return cancel.is_set() or asked() == 'stopped'

    def record(result):
        # Successful outcomes remain useful even after another branch fails or the user stops.
        # Canceled unfinished outcomes keep their original active identity for native recovery.
        if cancelled() and not result.submitted:
            preserve_partial(result)
            return
        if not runner._record(state, graph, run_dir, decided, result, settle):
            cancel.set()

    with ThreadPoolExecutor(max_workers=len(region.branches), thread_name_prefix='anchor-node') as pool:
        try:
            while True:
                if asked() == 'stopped':
                    cancel.set()
                if not cancel.is_set() and asked() is None and state.status == 'running':
                    busy = {step.node_id for step in futures.values()}
                    for branch in region.branches:
                        node = next((item for item in branch if item not in group['completed']), None)
                        if node is None or node in busy:
                            continue
                        # Region membership, not old edge timestamps, determines this activation's work.
                        step = runner._next_step(graph, state, decided, run_dir, [node],
                                                 lambda _: True, parallel=True)
                        if step is None or step.refused:
                            if step is not None:
                                runner._cease(state, step, settle, run_dir)
                                state.status, state.reason = 'stopped', 'max_rounds'
                            else:
                                if state.ceased and state.status == 'running':
                                    state.status, state.reason = 'stopped', 'max_rounds'
                                else:
                                    state.status = 'failed'
                                    group['failure'] = state.error or f'{node} cannot proceed'
                            state.save(run_dir)
                            cancel.set()
                            break
                        work = prepare(step, cancelled)
                        if cancelled() or asked() is not None:
                            break
                        futures[pool.submit(work)] = step
                if not futures:
                    break
                done, _ = wait(futures, timeout=0.1, return_when=FIRST_COMPLETED)
                for future in done:
                    futures.pop(future)
                    record(future.result())
        except BaseException as exc:  # noqa: BLE001 - cancel/drain even on interrupt, then re-raise
            error = exc
            cancel.set()
        finally:
            # Completed native facts survive even if recording one result raised. Do not let a
            # late worker mutate a workspace after the service has released this Run's ownership.
            for future in futures:
                try:
                    record(future.result())
                except BaseException as exc:  # noqa: BLE001 - drain all workers before re-raising
                    if error is None:
                        error = exc
                    cancel.set()
    if error is not None:
        raise error
    if state.status != 'running':
        state.save(run_dir)
        return False
    if runner._asked_to_stop(stop_request, state, run_dir) is not None:
        return False
    if state.active or any(node not in group['completed'] for branch in region.branches for node in branch):
        raise RuntimeError('parallel region has unfinished work without an active worker')
    return True
