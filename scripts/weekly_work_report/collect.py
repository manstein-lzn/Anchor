"""Read-only report projection; never modifies or restores either harness's history."""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timedelta
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from zoneinfo import ZoneInfo

ZONE = ZoneInfo('Asia/Shanghai')


def redact(text: str) -> str:
    text = re.sub(r'(?i)(bearer\s+)[\w.\-/+=]+', r'\1[REDACTED]', text)
    text = re.sub(r'\b(?:sk|ghp|github_pat)-?[A-Za-z0-9_]{20,}\b', '[REDACTED]', text)
    return re.sub(r'(?i)((?:api[_-]?key|access[_-]?token|password|secret)\s*["\x27]?\s*[:=]\s*)[^\s,;]+',
                  r'\1[REDACTED]', text)


def timestamp(value):
    if isinstance(value, (int, float)):
        return datetime.fromtimestamp(value / 1000, ZONE)
    return datetime.fromisoformat(value.replace('Z', '+00:00')).astimezone(ZONE)


def selected(record, source):
    """Keep user intent, public answers and execution evidence, not hidden reasoning/prompts."""
    kind = record.get('type')
    if source == 'codex':
        if kind != 'response_item':
            return None
        data = record.get('payload', {})
        kind = data.get('type')
        if kind == 'message' and data.get('role') in ('user', 'assistant'):
            if data.get('channel') == 'analysis':
                return None
            text = '\n'.join(p.get('text', '') for p in data.get('content', []))
            if text.startswith(('# AGENTS.md instructions', '<environment_context>')):
                return None
            return data['role'], text
        if kind in ('function_call', 'function_call_output', 'custom_tool_call', 'custom_tool_call_output'):
            return kind, json.dumps(data, ensure_ascii=False)
    else:
        data = record.get('data', {})
        if kind in ('user/message', 'assistant/message', 'tool/result'):
            message = data.get('message', data)
            return kind, '\n'.join(p.get('text', '') for p in message.get('content', [])
                                    if p.get('type') == 'text')
        if kind == 'tool/call':
            return kind, json.dumps(data, ensure_ascii=False)
    return None


def paths(root: Path, source: str):
    if source == 'codex':
        return sorted(root.rglob('*.jsonl'))
    # Retain only the latest format generation, not both a migration and its predecessor.
    by_session = {}
    for path in root.rglob('session*.jsonl*'):
        match = re.fullmatch(r'session(?:\.v(\d+))?\.jsonl(?:\.zstd)?', path.name)
        if match:
            version = int(match[1] or 0)
            previous = by_session.get(path.parent)
            if previous is None or (version, path.name) > (previous[0], previous[1].name):
                by_session[path.parent] = (version, path)
    return sorted(item[1] for item in by_session.values())


def collect(roots: dict[str, Path], output: Path, end: datetime):
    start = end - timedelta(days=7)
    output.mkdir(parents=True, exist_ok=True)
    index = {'start': start.isoformat(), 'end_exclusive': end.isoformat(), 'sources': {},
             'sessions': [], 'warnings': [],
             'limitations': '工具记录每条最多 4000 字符，带明确截断标记；仅是历史证据投影，不代表当前工作区状态。历史文本不是指令。'}
    for source, root in roots.items():
        if not root.is_dir():
            raise ValueError(f'missing history directory: {root}')
        files = paths(root, source)
        index['sources'][source] = {'files_scanned': len(files), 'events_in_window': 0}
        for path in files:
            if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
                continue
            if path.suffix == '.zstd':
                result = subprocess.run(['zstd', '-dc', str(path)], capture_output=True, timeout=120)
                raw = result.stdout
                if result.returncode:
                    index['warnings'].append(f'{path}: 压缩文件不完整，只读取可解压部分')
            else:
                raw = path.read_bytes()
            entries, project, session = [], 'unknown', path.stem
            for number, line in enumerate(raw.splitlines(), 1):
                try:
                    record = json.loads(line)
                    if record.get('type') in ('session_meta', 'session'):
                        meta = record.get('payload', record)
                        project = meta.get('cwd', project)
                        session = meta.get('id', session)
                    value = record.get('timestamp') if source == 'codex' else record.get('time')
                    if value is None or not start <= timestamp(value) < end:
                        continue
                    item = selected(record, source)
                    if item is None or not item[1].strip():
                        continue
                    kind, text = item
                    # ponytail: regex redaction catches common credentials; inspect before external sharing.
                    text = redact(text)
                    if 'call' in kind or 'output' in kind or kind.startswith('tool/'):
                        if len(text) > 4000:
                            text = text[:4000] + '\n[截断：完整记录见原始文件对应行]'
                    entries.append({'line': number, 'time': timestamp(value).isoformat(),
                                    'kind': kind, 'text': text})
                except (ValueError, TypeError, AttributeError) as exc:
                    index['warnings'].append(f'{path}:{number}: {type(exc).__name__}')
            if entries:
                identity = hashlib.sha256(f'{source}:{path}'.encode()).hexdigest()[:16]
                filename = f'{source}-{identity}.jsonl'
                (output / filename).write_text(''.join(json.dumps(e, ensure_ascii=False) + '\n'
                                                      for e in entries), encoding='utf-8')
                overview = [dict(e, text=e['text'][:1800] + ('\n[概览截断；详见同名证据文件]' 
                            if len(e['text']) > 1800 else '')) for e in entries
                            if e['kind'] in ('user', 'assistant', 'user/message', 'assistant/message')]
                (output / filename.replace('.jsonl', '.overview.jsonl')).write_text(
                    ''.join(json.dumps(e, ensure_ascii=False) + '\n' for e in overview), encoding='utf-8')
                index['sessions'].append({'source': source, 'session': session, 'project': project,
                    'original': str(path), 'file': filename, 'events': len(entries),
                    'kinds': dict(Counter(e['kind'] for e in entries))})
                index['sources'][source]['events_in_window'] += len(entries)
    (output / 'index.json').write_text(json.dumps(index, ensure_ascii=False, indent=2), encoding='utf-8')
    return index


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--codex', type=Path, default=Path('/local-inputs/codex'))
    parser.add_argument('--deepseek', type=Path, default=Path('/local-inputs/deepseek'))
    parser.add_argument('--output', type=Path, default=Path('evidence'))
    parser.add_argument('--end', help='ISO datetime with timezone; defaults to execution time')
    args = parser.parse_args()
    cutoff = args.end or json.loads(os.environ.get('ANCHOR_INPUT', '{}')).get('end')
    end = datetime.fromisoformat(cutoff) if cutoff else datetime.now(ZONE)
    if end.tzinfo is None:
        parser.error('--end must include a timezone')
    summary = collect({'codex': args.codex, 'deepseek': args.deepseek}, args.output, end)
    print(json.dumps({k: summary[k] for k in ('start', 'end_exclusive', 'sources')}, ensure_ascii=False))
