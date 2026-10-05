"""Real-provider channel tools acceptance using the existing local gateway RPC.

Default: real ControlServer/EventLedger with a local platform ACK fixture;
no external message. Retains services, artifacts and delivery evidence.
"""
from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import threading

from PIL import Image

from anchor.channel import EventLedger
from anchor.channel.control import ControlServer, request
from anchor.runtime.secrets import load_dotenv
from rust_channel_smoke import Deployment, GRAPH, ROOT
from rust_platform_plugin_smoke import Evidence, MODEL_KEYS, require


class Gateway:
    def __init__(self, root: Path):
        self.path = root / 'control.sock'
        self.token = 'local-acceptance-token'
        self.sent = []
        self.ready = threading.Event()
        self.loop = asyncio.new_event_loop()
        self.server = ControlServer(self.path, self.token, EventLedger(root / 'gateway'), self.send)
        self.thread = threading.Thread(target=self.run, daemon=True)

    async def send(self, user, message):
        self.sent.append({'user': user, 'message': message})
        return {'errcode': 0}

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_until_complete(self.server.start())
        self.ready.set()
        self.loop.run_forever()
        self.loop.run_until_complete(self.server.close())
        self.loop.close()

    def start(self):
        self.thread.start()
        require(self.ready.wait(5), 'Local gateway did not start')

    def stop(self):
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'rust/target/debug/anchor-runner-host')
    parser.add_argument('--timeout', type=float, default=180)
    args = parser.parse_args()
    load_dotenv(ROOT / '.env')
    require(all(os.environ.get(key) for key in MODEL_KEYS), 'Real provider configuration required')
    root = Path(tempfile.mkdtemp(prefix='rust-channel-tools-', dir=ROOT / '.local'))
    evidence = Evidence(root, tuple(os.environ[key] for key in MODEL_KEYS))
    gateway = Gateway(root)
    os.environ['ANCHOR_WECOM_SEND_USERS'] = 'alice'
    descriptor = root / 'control.json'
    descriptor.write_text(json.dumps({'socket': str(gateway.path), 'token': gateway.token}))
    descriptor.chmod(0o600)
    deployment = Deployment(evidence, args.binary.resolve(), rust_env={
        'ANCHOR_CHANNEL_CONTROL_DESCRIPTOR': str(descriptor), 'ANCHOR_WECOM_SEND_USERS': 'alice',
    })
    image = io.BytesIO()
    Image.new('RGB', (32, 32), 'red').save(image, format='PNG')
    png = image.getvalue()
    report = {'status': 'failed', 'scope': 'real provider; local gateway ACK fixture; no external delivery',
              'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest()}
    try:
        gateway.start()
        deployment.start()
        graph = json.loads((ROOT / 'examples/graphs/wecom-assistant.json').read_text())
        deployment.api.expect('POST', '/graphs', {'name': GRAPH, 'definition': graph}, status=201)
        prompt = ('验收宿主工具：请向 userid=alice 连续主动发送两次完全相同的正文「Rust channel acceptance」。'
                  '两次都是我的明确要求；每次调用成功后再进行下一次，不要在错误时重试。'
                  '然后用 anchor_run 执行 python3 -c 解码以下 base64 为 /workspace/result.png：'
                  + base64.b64encode(png).decode() + '。'
                  '用 wecom_attach_image 附加该图片，最后答复「发送及图片准备完成」。')
        event, reply = deployment.event('tools', 'alice', prompt, args.timeout)
        detail = deployment.check_run(reply)
        require(detail.get('channel_reply') is True, 'Missing public rich reply indicator')
        require(len(gateway.sent) == 2, 'Expected two intentionally identical proactive sends')
        require(gateway.sent[0] == gateway.sent[1], 'Expected identical recipient and content')
        require(base64.b64decode(reply['msg_item'][0]['image']['base64']) == png, 'Wrong reply image bytes')
        require(reply['msg_item'][0]['image']['md5'] == hashlib.md5(png).hexdigest(), 'Wrong platform image digest')
        deployment.stop()
        deployment.start()
        _, duplicate = deployment.event('after-restart', 'alice', '', args.timeout, event=event)
        require(reply == duplicate and len(gateway.sent) == 2, 'Restart replayed send or lost rich reply')
        # Exercise the same gateway's uncertain-ACK contract without a model.
        async def uncertain_send(user, message):
            raise ConnectionError('fixture lost ACK')
        gateway.server.send = uncertain_send
        uncertain_payload = {'operation': 'send', 'request_id': 'fixture-unknown', 'userid': 'alice', 'content': 'unknown'}
        for _ in range(2):
            try:
                request(gateway.path, gateway.token, uncertain_payload)
            except RuntimeError:
                pass
            else:
                raise AssertionError('Unknown delivery reported success')
        report.update(status='passed', run=reply['run'], deliberate_sends=len(gateway.sent),
                      image_sha256=hashlib.sha256(png).hexdigest(), restart_reply_equal=True)
        evidence.save('deliveries.json', gateway.sent)
    finally:
        deployment.stop()
        gateway.stop()
        evidence.save('evidence.json', report)
        print(json.dumps({'status': report['status'], 'evidence': str(root / 'evidence.json')}))


if __name__ == '__main__':
    main()
