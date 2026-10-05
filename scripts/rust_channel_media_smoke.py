"""Real model acceptance for frozen channel files and native image input.

Uses disposable services and normalized private events. No production gateway
or outgoing WeCom messages. The configured model must support image input.
"""
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
from uuid import uuid4

from PIL import Image, ImageDraw, ImageFont

from rust_channel_smoke import Deployment, GRAPH, ROOT
from rust_platform_plugin_smoke import Evidence, MODEL_KEYS, require, wait_until


def picture(token: str) -> bytes:
    image = Image.new('RGB', (800, 360), 'white')
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf', 76)
    draw.text((55, 30), token, fill='black', font=font)
    draw.rectangle((75, 170, 210, 305), fill='#e53935')
    draw.ellipse((330, 170, 465, 305), fill='#1565c0')
    out = io.BytesIO()
    image.save(out, format='PNG')
    return out.getvalue()


def attachment_event(deployment: Deployment, user: str, image: bytes, secret: str) -> tuple[dict, list[Path]]:
    event_id = str(uuid4())
    directory = deployment.paths['platform'] / 'state/channels/wecom/events' / event_id
    directory.mkdir(parents=True)
    files = [directory / 'picture.png', directory / 'document.txt']
    files[0].write_bytes(image)
    files[1].write_text('Document verification code: ' + secret + '\n')
    event = {
        'source': 'wecom', 'event_id': event_id, 'sender_id': user, 'conversation_id': user,
        'message_type': 'mixed', 'metadata': {'chat_type': 'single'},
        'text': '本次验收请直接看附图，辨认图中的英文数字口令，并说明两个彩色图形。'
                '用 anchor_run 读取 /in/channel/document.txt 中的文件口令，'
                '把图中口令和文件口令写入 /workspace/checked.txt。'
                '复制 /in/channel/document.txt 到 /workspace/document.txt。'
                '最终答复包含图中口令、文件口令、图形颜色和形状。不要发送外部消息。',
        'attachments': [
            {'kind': 'image', 'name': 'picture.png', 'path': str(files[0]), 'mime_type': 'image/png'},
            {'kind': 'file', 'name': 'document.txt', 'path': str(files[1]), 'mime_type': 'text/plain'},
        ],
    }
    return event, files


def verify_images(deployment: Deployment, reply: dict, expected: bytes, forbidden: bytes) -> int:
    record = json.loads((deployment.paths['state'] / 'runs' / (reply['run'] + '.json')).read_text())
    key = record['results']['assistant'][0]['key']
    durable = ':'.join(str(key[name]) for name in ('run_id', 'graph_digest', 'node_id', 'invocation'))
    stem = 'np1-' + hashlib.sha256(durable.encode()).hexdigest()
    requests = sorted((deployment.paths['state'] / 'io-harness/store' / (stem + '.recordings')).glob('*/rig-request.json'))
    wanted, denied = base64.b64encode(expected).decode(), base64.b64encode(forbidden).decode()
    require(len(requests) >= 2, 'Expected image input to survive multiple model requests')
    for request in requests:
        text = request.read_text()
        require(wanted in text and denied not in text, 'Wrong or missing frozen image bytes in model request')
    return len(requests)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'rust/target/release/anchor-runner-host')
    parser.add_argument('--timeout', type=float, default=180)
    args = parser.parse_args()
    require(all(os.environ.get(key) for key in MODEL_KEYS), 'Real visual model configuration required')
    os.environ['ANCHOR_MODEL_IMAGE_MODELS'] = json.dumps([os.environ['ANCHOR_MODEL_NAME']])
    root = Path(tempfile.mkdtemp(prefix='rust-channel-media-', dir=ROOT / '.local'))
    evidence = Evidence(root, tuple(os.environ[key] for key in MODEL_KEYS))
    deployment = Deployment(evidence, args.binary.resolve())
    report = {'status': 'failed', 'scope': 'files and native image input; no external delivery',
              'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest()}
    try:
        deployment.start()
        graph = json.loads((ROOT / 'examples/graphs/wecom-assistant.json').read_text())
        deployment.api.expect('POST', '/graphs', {'name': GRAPH, 'definition': graph}, status=201)
        require(deployment.api.expect('GET', f'/graphs/{GRAPH}')['definition'] == graph, 'Original Graph changed')
        cases = {}
        for user in ('alice', 'bob'):
            token, secret = uuid4().hex[:6].upper(), uuid4().hex
            data = picture(token)
            evidence.root.joinpath(f'{user}.png').write_bytes(data)
            event, paths = attachment_event(deployment, user, data, secret)
            cases[user] = {'token': token, 'secret': secret, 'image': data, 'event': event, 'paths': paths}
        with ThreadPoolExecutor(max_workers=2) as pool:
            pending = {user: pool.submit(deployment.event, user, user, '', args.timeout, event=case['event'])
                       for user, case in cases.items()}
            wait_until(lambda: len(deployment.rust.expect('GET', '/runs')['runs']) == 2, 30, 'attachment admission')
            # All later model requests and reads must use Rust's frozen bytes.
            for case in cases.values():
                for path in case['paths']:
                    path.unlink()
            replies = {user: task.result()[1] for user, task in pending.items()}
        model_requests = 0
        for user, case in cases.items():
            reply = replies[user]
            other = cases['bob' if user == 'alice' else 'alice']
            deployment.check_run(reply)
            require(case['token'] in reply['text'] and case['secret'] in reply['text'],
                    'Model did not read the actual image and document codes')
            require('红色正方形' in reply['text'] and '蓝色圆形' in reply['text'],
                    'Model did not identify both colored shapes')
            require(other['token'] not in reply['text'] and other['secret'] not in reply['text'], 'User content leaked')
            status, data = deployment.api.raw('GET', f"/runs/{reply['run']}/files/assistant/checked.txt?download=1")
            require(status == 200 and case['token'] in data.decode() and case['secret'] in data.decode(), 'Artifact differs from input')
            status, data = deployment.api.raw('GET', f"/runs/{reply['run']}/files/assistant/document.txt?download=1")
            require(status == 200 and data == ('Document verification code: ' + case['secret'] + '\n').encode(),
                    'Copied document differs from the frozen input bytes')
            model_requests += verify_images(deployment, reply, case['image'], other['image'])
            detail = deployment.rust.expect('GET', f"/runs/{reply['run']}")
            require(len(detail['attachments']) == 2, 'Missing public attachment manifest')
            _, duplicate = deployment.event(user + '-duplicate', user, '', args.timeout, event=case['event'])
            require(duplicate == reply, 'Event retry depended on deleted original attachments')
        require(len(deployment.rust.expect('GET', '/runs')['runs']) == 2, 'Duplicate event reran the model')
        deployment.stop()
        deployment.start()
        for user, case in cases.items():
            _, reply = deployment.event(user + '-restart', user,
                '继续上轮：从记忆说出上轮图片口令和文件口令。用anchor_run读取 /previous/checked.txt核对，'
                '将它复制到本轮/workspace/checked.txt。答复包含两个口令。不要发送外部消息。', args.timeout)
            deployment.check_run(reply)
            require(case['token'] in reply['text'] and case['secret'] in reply['text'], 'Restart lost the attachment conversation')
            other = cases['bob' if user == 'alice' else 'alice']
            require(other['token'] not in reply['text'] and other['secret'] not in reply['text'],
                    'User content leaked after restart')
        require(len(deployment.rust.expect('GET', '/runs')['runs']) == 4, 'Unexpected Run count after restart')
        require(not list((deployment.paths['platform'] / 'workspaces').glob('*/runs/*/run.json')), 'Python wrote a Graph Run')
        report.update(status='passed', graph_unchanged=True, users=2, runs=4,
                      native_image_requests=model_requests, original_files_removed_after_admission=True,
                      duplicate_after_source_removal=True, restart=True)
    except Exception as exc:
        report['error'] = str(exc)
        raise
    finally:
        deployment.stop()
        evidence.save('evidence.json', report)
        print(root / 'evidence.json', flush=True)


if __name__ == '__main__':
    main()
