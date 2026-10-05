"""Existing Session arbitration over Rust-owned background Graph calls."""
from __future__ import annotations

import hashlib
import json
import os
import threading
import time
from typing import Any

from anchor.channel.background import _Lease
from anchor.channel.runtime import POLL_INTERVAL, SETTLED, _json, _owned
from anchor.runtime_http import RuntimeHTTPError, resource_path


class _DeliveryPending(RuntimeError):
    """The gateway accepted the reply; only Rust settlement remains retryable."""


def _context(scheduler: Any, session_id: str, graph: str) -> dict:
    session = scheduler.sessions.get(session_id)
    if session.status == 'archived' or not session.channel or session.graph != graph:
        raise ValueError('call.session must be an active channel conversation bound to the target Graph')
    channel = session.channel
    if channel.get('source') != 'wecom':
        raise ValueError('unsupported session channel')
    allowed = {item.strip() for item in (os.environ.get('ANCHOR_WECOM_SEND_USERS') or
               os.environ.get('ANCHOR_WECOM_USERS', '')).split(',') if item.strip()}
    if channel.get('sender_id') not in allowed and '*' not in allowed:
        raise ValueError('session recipient is not allowed')
    return {'session': session.id, 'reply_node': session.reply_node,
            'conversation_id': session.conversation_id, 'channel': channel}


def _ancestry(scheduler: Any, parent: str, graph: str) -> None:
    seen = set()
    while parent:
        if parent in seen:
            raise ValueError('cyclic call ancestry')
        seen.add(parent)
        source = scheduler.run('', parent)
        if source is None or source.get('backend') != 'rust':
            raise ValueError('Graph call source is missing')
        trigger = source['state']['trigger']
        if trigger.get('session') or source.get('session_call'):
            raise ValueError('a conversation cannot enqueue work into a Session')
        if source['graph'] == graph:
            raise ValueError('recursive Graph call is unsupported')
        parent = trigger.get('run') if trigger.get('source') == 'graph_call' else None


def resolve(scheduler: Any, body: dict) -> tuple[str, int]:
    try:
        if (scheduler.runtime is None or not isinstance(body, dict) or
                set(body) != {'parent_run', 'session', 'graph'} or
                any(not isinstance(value, str) or not value.strip() for value in body.values())):
            raise ValueError('invalid Session call request')
        context = _context(scheduler, body['session'], body['graph'])
        graph = _json(*scheduler.graph(body['graph']))
        if context['reply_node'] not in graph.get('node_plugins', {}):
            raise ValueError('the session reply node no longer exists')
        _ancestry(scheduler, body['parent_run'], body['graph'])
        return json.dumps(context), 200
    except (KeyError, ValueError) as exc:
        return json.dumps({'error': str(exc)}), 400
    except RuntimeHTTPError as exc:
        return exc.response()


def _request(scheduler: Any, run: str, operation: str, body: dict | None = None) -> dict:
    value, status = scheduler.runtime.request('POST', resource_path('runs', run) + '/' + operation, body)
    if status not in {200, 202}:
        raise RuntimeHTTPError(value.get('error') or 'Session call request failed', status)
    return value


def _yield(scheduler: Any, run: str) -> dict:
    while True:
        try:
            snapshot = scheduler.run('', run)
            if (snapshot is not None and snapshot.get('active') is False and
                    snapshot['state']['status'] in SETTLED | {'budget_stopped'}):
                return snapshot
            _request(scheduler, run, 'session-yield')
        except (RuntimeHTTPError, OSError):
            # An unanswered yield is not evidence that the native Session is free.
            # Keep the foreground predecessor blocked until inactivity is observed.
            pass
        time.sleep(POLL_INTERVAL)


def _previous(scheduler: Any, session: Any, run: str) -> str | None:
    candidates = []
    for item in scheduler.runs():
        trigger = item.get('trigger', {})
        if item.get('backend') == 'rust' and trigger.get('session') == session.id:
            snapshot = scheduler.run('', item['run'])
            _owned(snapshot, session)
            candidates.append(snapshot)
    referenced = {item['state']['trigger'].get('previous_run') for item in candidates}
    heads = [item['run'] for item in candidates if item['run'] not in referenced]
    if len(heads) > 1:
        raise RuntimeHTTPError('conversation lineage has multiple heads', 409)
    return heads[0] if heads and heads[0] != run else None


def _delivery_key(identifier: str) -> str:
    return hashlib.sha256(f'graph-call-reply:{identifier}'.encode()).hexdigest()


def _accepted_delivery(scheduler: Any, snapshot: dict) -> str | None:
    """Return a durable ACK projection without revalidating its now-current recipient.

    The Session event is the receipt of the external side effect.  Once it is
    present, a later archive/recipient-policy change must not cause another
    gateway send or turn an already accepted reply into a failed call.
    """
    call = snapshot.get('session_call') or {}
    context = call.get('context') or {}
    session_id = context.get('session')
    identifier = snapshot.get('run')
    if not isinstance(session_id, str) or not isinstance(identifier, str):
        return None
    request_id = _delivery_key(identifier)
    for event in scheduler.sessions.events(session_id):
        data = getattr(event, 'data', {})
        if (getattr(event, 'kind', '') == 'graph.call.delivered' and
                isinstance(data, dict) and data.get('run') == identifier and
                data.get('request_id') == request_id and data.get('accepted') is True):
            return request_id
    return None


def _settle_accepted(scheduler: Any, identifier: str) -> None:
    """Settle a reply whose gateway ACK was already projected to Session history."""
    try:
        _request(scheduler, identifier, 'session-settlement', {'status': 'delivered'})
    except Exception as exc:  # noqa: BLE001 - a known ACK must stay retryable, never failed
        raise _DeliveryPending('accepted reply awaits Session settlement') from exc


def _acquire(scheduler: Any, session_id: str, identifier: str) -> _Lease | None:
    with scheduler.lock:
        tail = scheduler.channel_tail.get(session_id)
        background = scheduler.session_background.get(session_id)
        if not tail and background is None:
            lease = _Lease(run_id=identifier)
            scheduler.session_background[session_id] = lease
            return lease
        predecessor = tail[1] if tail else background.released
    predecessor.wait(.1)
    return None


def _release(scheduler: Any, session_id: str, lease: _Lease) -> None:
    _yield(scheduler, lease.run_id)
    with scheduler.lock:
        if scheduler.session_background.get(session_id) is lease:
            scheduler.session_background.pop(session_id)
        lease.released.set()


def _deliver(scheduler: Any, snapshot: dict, session: Any, lease: _Lease) -> bool:
    identifier = lease.run_id
    context = snapshot['session_call']['context']
    # Serialize final permission/cancellation check with foreground admission.
    with scheduler.lock:
        if lease.interrupted.is_set():
            return False
        if _context(scheduler, session.id, snapshot['graph']) != context:
            raise ValueError('the bound channel conversation changed before delivery')
        snapshot = scheduler.run('', identifier)
        if snapshot is None:
            raise RuntimeHTTPError('background Run disappeared before delivery', 409)
        if snapshot['session_call']['status'] != 'pending':
            return True
        if snapshot.get('active') is not False or snapshot['state']['status'] != 'completed':
            return False
        result = snapshot['state']['nodes'].get(session.reply_node, {})
        text = result.get('submission')
        if not result.get('submitted') or not isinstance(text, str) or not text.strip():
            raise ValueError('the assistant reply node did not produce a reply')
        if scheduler.channel_supervisor is None:
            raise RuntimeError('WeCom gateway is unavailable')
        key = _delivery_key(identifier)
        receipt = scheduler.channel_supervisor.send('wecom', {
            'operation': 'send', 'request_id': key,
            'userid': context['channel']['sender_id'], 'content': text})
        if not receipt.get('accepted'):
            raise RuntimeError('platform did not confirm the assistant reply')
        try:
            # Persist the accepted external effect before the Rust mutation. A
            # restart can then settle this exact Run without current recipient
            # validation or another gateway request.
            scheduler.sessions.append(session.id, 'graph.call.delivered', {
                'run': identifier, 'request_id': key, 'accepted': True})
            _request(scheduler, identifier, 'session-settlement', {'status': 'delivered'})
        except Exception as exc:  # noqa: BLE001 - gateway ACK must never become a failed settlement
            raise _DeliveryPending('accepted reply awaits Session settlement') from exc
    return True


def _wait_execution(scheduler: Any, session: Any, lease: _Lease, previous: str | None) -> bool:
    while True:
        snapshot = scheduler.run('', lease.run_id)
        if snapshot is None:
            raise RuntimeHTTPError('accepted background Run disappeared', 409)
        if lease.interrupted.is_set():
            return False
        if snapshot['session_call']['status'] != 'pending':
            return True
        if snapshot.get('active') is False:
            status = snapshot['state']['status']
            if status == 'waiting_recovery':
                _request(scheduler, lease.run_id, 'session-execution', {
                    'session': session.id, 'previous_run': previous})
            elif status == 'completed':
                return _deliver(scheduler, snapshot, session, lease)
            elif status in {'failed', 'aborted', 'budget_stopped', 'stopped'}:
                raise RuntimeError('background execution ' + status)
        time.sleep(POLL_INTERVAL)


def _execute_leased(scheduler: Any, snapshot: dict, lease: _Lease) -> bool:
    context = snapshot['session_call']['context']
    session_id = context['session']
    if _context(scheduler, session_id, snapshot['graph']) != context:
        raise ValueError('the bound channel conversation changed after admission')
    session = scheduler.sessions.get(session_id)
    previous = snapshot['state']['trigger'].get('previous_run')
    if not snapshot['state']['trigger'].get('session'):
        previous = _previous(scheduler, session, lease.run_id)
    if lease.interrupted.is_set():
        return False
    _request(scheduler, lease.run_id, 'session-execution', {'session': session_id, 'previous_run': previous})
    scheduler.sessions.attach_run(session_id, lease.run_id)
    scheduler.sessions.append(session_id, 'graph.call.started', {'run': lease.run_id})
    return _wait_execution(scheduler, session, lease, previous)


def _coordinate(scheduler: Any, identifier: str) -> None:
    while True:
        snapshot = scheduler.run('', identifier)
        if snapshot is None or not snapshot.get('session_call') or snapshot['session_call']['status'] != 'pending':
            return
        if _accepted_delivery(scheduler, snapshot) is not None:
            _settle_accepted(scheduler, identifier)
            return
        session_id = snapshot['session_call']['context']['session']
        lease = _acquire(scheduler, session_id, identifier)
        if lease is None:
            continue
        try:
            done = _execute_leased(scheduler, snapshot, lease)
        finally:
            _release(scheduler, session_id, lease)
        if done:
            return


def execute(scheduler: Any, identifier: str) -> None:
    """Retain one child Run while yielding the existing Session slot to user messages."""
    try:
        _coordinate(scheduler, identifier)
    except _DeliveryPending:
        # Retry settlement on the next pending scan using the gateway's receipt.
        pass
    except Exception as exc:  # noqa: BLE001 - execution and delivery facts remain Rust/gateway-owned
        if isinstance(exc, RuntimeHTTPError) and exc.status >= 500:
            # The response may have been lost after Rust accepted execution.
            # Lease cleanup already waited for inactivity; preserve the pending call.
            return
        try:
            _yield(scheduler, identifier)
            _request(scheduler, identifier, 'session-settlement', {'status': 'failed', 'error': str(exc)})
        except (RuntimeHTTPError, OSError):
            # A disconnected host is retried by the next scan; never replace the Run.
            pass
    finally:
        with scheduler.lock:
            scheduler.session_call_workers.discard(identifier)


def tick(scheduler: Any) -> None:
    if scheduler.runtime is None:
        return
    try:
        pending = [item['run'] for item in scheduler.runs()
                   if item.get('backend') == 'rust' and (item.get('session_call') or {}).get('status') == 'pending']
    except RuntimeHTTPError:
        return
    with scheduler.lock:
        for identifier in pending:
            if identifier not in scheduler.session_call_workers:
                scheduler.session_call_workers.add(identifier)
                threading.Thread(target=execute, args=(scheduler, identifier), daemon=True).start()
