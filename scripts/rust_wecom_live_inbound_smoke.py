"""Verify real WeCom ingress, Rust execution and acknowledged final delivery.

Use --prepare-only while the existing bot is online. For live acceptance, stop
its existing gateway first, pass --user, then send the displayed test prompt.
This script does not change the existing deployment or replay an inbound event.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import tempfile

from anchor.runtime.secrets import load_dotenv
from rust_channel_smoke import GRAPH, ROOT
from rust_platform_plugin_smoke import (
    Api, Evidence, MODEL_KEYS, Service, require, service_envs, unused_ports, wait_until,
)

PROMPT = "Rust入站验收1005：请用工具生成一张红色方块PNG图片，附图回复，并在正文写出验收口令 RUST-1005。"


def require_no_existing_gateway() -> None:
    """Do not compete with the same bot's existing connection on this host."""
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            if b'ws_gateway.py' not in (process / 'cmdline').read_bytes():
                continue
            env = dict(item.split(b'=', 1) for item in (process / 'environ').read_bytes().split(b'\0') if b'=' in item)
        except (FileNotFoundError, ProcessLookupError):
            continue
        require(env.get(b'WECOM_BOT_ID') != os.environ['WECOM_BOT_ID'].encode(),
                'The same WeCom bot already has a gateway; stop it before live acceptance')


class LiveDeployment:
    def __init__(self, evidence: Evidence, binary: Path, user: str):
        self.evidence, self.binary, self.user = evidence, binary, user
        self.paths = {name: evidence.root / name for name in ('platform', 'bootstrap', 'catalog', 'state', 'work')}
        self.paths['library'] = self.paths['platform'] / 'library'
        shutil.copytree(ROOT / 'plugins/wecom', self.paths['library'] / 'plugins/wecom')
        self.paths['bootstrap'].mkdir()
        self.paths['catalog'].mkdir()
        (self.paths['bootstrap'] / 'graph.json').write_text(json.dumps({
            'entry': 'start', 'agents': {}, 'ops': {'start': {'run': 'true'}},
            'nodes': [{'id': 'start', 'op': 'start'}], 'edges': [],
        }))
        (self.paths['bootstrap'] / 'manifest.json').write_text('{"format":1,"graph":"graph.json","plugins":[]}')
        (evidence.root / 'runtime.json').write_text('{}\n')
        self.services: list[Service] = []

    def start(self, *, live: bool) -> None:
        rust_port, platform_port = unused_ports()
        rust, platform = service_envs(self.paths, ROOT / 'src', rust_port)
        rust.update({
            'ANCHOR_CHANNEL_CONTROL_DESCRIPTOR': str(self.paths['platform'] / 'state/channels/wecom/control.json'),
            'ANCHOR_WECOM_SEND_USERS': self.user,
            'ANCHOR_RUNNER_ALLOWED_COMMANDS': 'sh,cat,printf,cp,python3',
        })
        token = secrets.token_urlsafe(32)
        self.evidence.secrets += (token,)
        platform.update({
            'ANCHOR_API_KEYS': json.dumps([token]), 'ANCHOR_API_KEY': token,
            'ANCHOR_WECOM_GRAPH': GRAPH, 'ANCHOR_WECOM_REPLY_NODE': 'assistant',
            'ANCHOR_WECOM_USERS': self.user, 'ANCHOR_WECOM_SEND_USERS': self.user,
        })
        if live:
            require_no_existing_gateway()
            platform.update({name: os.environ[name] for name in ('WECOM_BOT_ID', 'WECOM_BOT_SECRET')})
        self.rust, self.platform = Api(rust_port), Api(platform_port)
        self.platform.opener.addheaders = [('Authorization', 'Bearer ' + token)]
        self.services.append(Service('rust', [str(self.binary), 'serve'], rust, self.evidence))
        wait_until(lambda: self.rust.ready(self.services[0], '/health'), 30, 'Rust readiness')
        self.services.append(Service('platform', [str(ROOT / '.venv/bin/python'), '-m', 'anchor',
            '--root', str(self.paths['platform']), '--config', str(self.evidence.root / 'runtime.json'),
            '--host', '127.0.0.1', '--port', str(platform_port)], platform, self.evidence))
        wait_until(lambda: self.platform.ready(self.services[1], '/graphs'), 30, 'platform readiness')
        graph = json.loads((ROOT / 'examples/graphs/wecom-assistant.json').read_text())
        self.platform.expect('POST', '/graphs', {'name': GRAPH, 'definition': graph}, status=201)
        require(self.rust.expect('GET', f'/graphs/{GRAPH}')['definition'] == graph, 'Original Graph changed')
        require(self.platform.expect('GET', f'/graphs/{GRAPH}')['definition'] == graph, 'Platform Graph differs')
        if live:
            wait_until(lambda: 'Authentication successful' in (self.evidence.root / 'platform.log').read_text(),
                       30, 'WeCom authentication')

    def wait_for_delivery(self, timeout: float) -> dict:
        def completed():
            for service in self.services:
                service.check()
            runs = self.rust.expect('GET', '/runs').get('runs', [])
            matching = [item for item in runs if item.get('graph') == GRAPH]
            if not matching:
                return None
            latest = max(matching, key=lambda item: item.get('updated', ''))
            require(latest['status'] not in {'failed', 'stopped', 'aborted', 'waiting_recovery'},
                    'Rust Run did not complete; inspect retained Run and native records')
            if latest['status'] != 'completed':
                return None
            detail = self.platform.expect('GET', f"/runs/{latest['run']}")
            return detail if not detail.get('active') else None

        detail = wait_until(completed, timeout, 'the Rust channel Run')
        self.evidence.save('rust-run.json', detail)
        require(detail.get('backend') == 'rust', 'The Run must be Rust-owned')
        require(detail['state']['input']['channel']['sender_id'] == self.user, 'Unexpected sender')

        def acknowledged():
            path = self.paths['platform'] / 'state/channels/wecom/events.sqlite'
            with sqlite3.connect(f'file:{path}?mode=ro', uri=True) as db:
                rows = db.execute('SELECT event_id,status,reply,error FROM channel_events WHERE source=? AND sender_id=?',
                                  ('wecom', self.user)).fetchall()
            for identifier, status, saved, error in rows:
                reply = json.loads(saved).get('channel_reply', {}) if saved else {}
                if reply.get('run') != detail['run']:
                    continue
                require(not error, 'Gateway delivery failed; inspect retained ledger')
                if status == 'completed':
                    return {'event_id': identifier, 'status': status, 'reply': reply}
            return None

        delivery = wait_until(acknowledged, 30, 'the final WeCom platform ACK')
        self.evidence.save('delivery.json', delivery)
        require(detail.get('channel_reply') is True, 'Rust reply image was not prepared')
        images = self.rust.expect('GET', f"/runs/{detail['run']}/channel-reply")
        require(len(images) == 1 and images[0]['msgtype'] == 'image', 'Expected one PNG reply')
        image = base64.b64decode(images[0]['image']['base64'], validate=True)
        require(image.startswith(b'\x89PNG\r\n\x1a\n'), 'Expected a PNG')
        require(hashlib.md5(image).hexdigest() == images[0]['image']['md5'], 'Image digest mismatch')
        (self.evidence.root / 'reply.png').write_bytes(image)

        require(delivery['reply'].get('msg_item') == images, 'Delivered image differs from Rust image')
        require('rust-1005' in delivery['reply'].get('text', '').casefold(), 'Final reply omitted the test code')
        ledger = self.paths['platform'] / 'state/channels/wecom/events.sqlite'
        with sqlite3.connect(f'file:{ledger}?mode=ro', uri=True) as db:
            image_ack = db.execute('SELECT status FROM channel_events WHERE source=? AND event_id=?',
                ('wecom-reply-image', f"{delivery['event_id']}:0")).fetchone()
        require(image_ack == ('completed',), 'Separate image message was not acknowledged')
        return {'run': detail['run'], 'backend': 'rust', 'event_id': delivery['event_id'],
                'platform_ack': True, 'image_message_ack': True, 'image_transport': 'uploaded_media_reply',
                'image_sha256': hashlib.sha256(image).hexdigest(), 'user_receipt': 'pending'}

    def stop(self) -> None:
        for service in reversed(self.services):
            service.stop()
        self.services.clear()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'rust/target/debug/anchor-runner-host')
    parser.add_argument('--timeout', type=float, default=900)
    parser.add_argument('--prepare-only', action='store_true', help='Verify both services without connecting the bot')
    parser.add_argument('--user', help='Explicit private WeCom userid for this acceptance')
    args = parser.parse_args()
    load_dotenv(ROOT / '.env')
    required = MODEL_KEYS if args.prepare_only else (*MODEL_KEYS, 'WECOM_BOT_ID', 'WECOM_BOT_SECRET')
    require(all(os.environ.get(key) for key in required), 'model and WeCom configuration are required')
    require(args.prepare_only or bool(args.user and args.user not in {'*', '@all'}), 'Live acceptance needs one --user')
    if not args.prepare_only:
        require_no_existing_gateway()
    root = Path(tempfile.mkdtemp(prefix='rust-wecom-inbound-', dir=ROOT / '.local'))
    evidence = Evidence(root, tuple(os.environ.get(key, '') for key in MODEL_KEYS + ('WECOM_BOT_SECRET',)))
    report = {'status': 'failed', 'scope': 'real WeCom ingress -> Python adapter -> Rust Run -> final image reply',
              'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest()}
    deployment = LiveDeployment(evidence, args.binary.resolve(), args.user or 'prepare-only')
    try:
        deployment.start(live=not args.prepare_only)
        if args.prepare_only:
            report.update(status='prepared', scope='services and original Graph only; no bot/model request')
        else:
            print(json.dumps({'ready': True, 'prompt': PROMPT, 'evidence': str(root)}, ensure_ascii=False), flush=True)
            report.update(deployment.wait_for_delivery(args.timeout), status='platform_accepted')
    except Exception as exc:
        report.update(status='failed', error=type(exc).__name__)
        raise
    finally:
        deployment.stop()
        evidence.save('evidence.json', report)
        print(json.dumps({'status': report['status'], 'evidence': str(root / 'evidence.json')}, ensure_ascii=False), flush=True)


if __name__ == '__main__':
    main()
