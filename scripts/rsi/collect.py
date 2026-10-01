"""Read-only, uncapped RSI evidence collector (stdlib; runnable in an Op sandbox).

Inventories are authoritative about coverage, not provider acceptance. No conversation,
trace, environment secrets, or model reasoning is collected. Snapshots are redacted
review evidence, never executable replacements or a recovery store.
"""
from __future__ import annotations

import argparse
import ast
from collections import Counter
from datetime import datetime, timedelta
import hashlib
import importlib.metadata
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tokenize
from typing import Any
from zoneinfo import ZoneInfo

DEFAULT_ZONE = "Asia/Shanghai"
EXCLUDED_DIRS = {".git", ".env", ".venv", "venv", "node_modules", "dist", "build", "__pycache__",
                 ".cache", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".local", "state",
                 "workspaces", ".credentials", ".ssh", ".aws", "coverage", "htmlcov"}
SECRET_KEY = re.compile(r"(?:secret|password|passwd|token|api[_-]?key|credential|private[_-]?key|authorization)", re.I)
REFERENCE_KEY = re.compile(r"(?:env(?:ironment)?(?:_vars?)?|_env_var|_ref|_reference|_name|_path|_required)$", re.I)
ASSIGNMENT = re.compile(
    r'''(?ix)(?P<prefix>["']?\b[\w.-]*(?:secret|password|passwd|api[_-]?key|access[_-]?token|auth[_-]?token|token|credential|private[_-]?key|authorization)[\w.-]*["']?[ \t]*[:=][ \t]*)(?P<value>"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;\}\]\r\n]+)''')


def _reference(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"\$\{[^}]+\}|\$[A-Z_][A-Z0-9_]*", value))


def _text(text: str) -> str:
    text = re.sub(r'''(?is)([\w.-]*(?:secret|password|token|api[_-]?key|credential)[\w.-]*\s*[:=]\s*)("""|\x27\x27\x27).*?\2''',
                  lambda match: match[1] + '"[REDACTED]"', text)
    text = re.sub(r'(?im)^([ \t]*[\w.-]*(?:secret|password|token|api[_-]?key|credential)[\w.-]*\s*:\s*)[|>][-+]?[^\n]*\n(?:[ \t]+[^\n]*(?:\n|$))+',
                  lambda match: match[1] + '"[REDACTED]"\n', text)
    text = re.sub(r'(?i)\b(Bearer|Basic)\s+(?!\$)[A-Za-z0-9+/_.=-]{8,}', r'\1 [REDACTED]', text)
    def assignment(match: re.Match) -> str:
        value = match['value']
        plain = value.strip("\"'")
        prefix_key = match['prefix'].split('=')[0].split(':')[0].strip(" \"'")
        if REFERENCE_KEY.search(prefix_key) or _reference(plain):
            return match.group()
        # Keep code references and logic; only replace literal assignments.
        if not value.startswith(('"', "'")) and (plain.startswith(('os.', 'getenv(', 'self.', 'config.', 'None', 'False', 'True')) or '(' in plain or re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*\.[A-Za-z0-9_.]+', plain)):
            return match.group()
        return match['prefix'] + '"[REDACTED]"'
    text = ASSIGNMENT.sub(assignment, text)
    text = re.sub(r'(?i)\b(Bearer|Basic)\s+(?!\$)[A-Za-z0-9+/_.=-]{8,}', r'\1 [REDACTED]', text)
    text = re.sub(r'(https?://)[^\s/@:]+:[^\s/@]+@', r'\1[REDACTED]@', text)
    text = re.sub(r'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----', '[REDACTED PRIVATE KEY]', text, flags=re.S)
    return text


def _redact(value: Any, key: str = '') -> Any:
    if SECRET_KEY.search(key) and not REFERENCE_KEY.search(key) and not _reference(value):
        return '[REDACTED]'
    if isinstance(value, dict):
        return {str(k): _redact(v, str(k)) for k, v in value.items()}
    if isinstance(value, list):
        return [_redact(v) for v in value]
    return _text(value) if isinstance(value, str) else value


def _python_text(text: str) -> str:  # noqa: C901 - literal-only Python syntax projection
    """Redact literal values at Python AST spans without rewriting names, types or imports."""
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return _text(text)
    replacements = {}
    lines = text.encode().splitlines(keepends=True)
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line))

    def replace_literal(node, force=False):
        if not isinstance(node, ast.Constant) or not isinstance(node.value, (str, bytes)):
            return
        if isinstance(node.value, bytes):
            clean = b'[REDACTED]' if force else node.value
        else:
            clean = '[REDACTED]' if force and not _reference(node.value) else _text(node.value)
        if clean != node.value:
            begin = offsets[node.lineno - 1] + node.col_offset
            end = offsets[node.end_lineno - 1] + node.end_col_offset
            if force or (begin, end) not in replacements:
                replacements[(begin, end)] = b'(' + repr(clean).encode() + b'\n' * (node.end_lineno - node.lineno) + b')'

    def sensitive(name):
        return bool(SECRET_KEY.search(name) and not REFERENCE_KEY.search(name))

    def target_name(target):
        if isinstance(target, ast.Name):
            return target.id
        if isinstance(target, ast.Attribute):
            return target.attr
        if isinstance(target, ast.Subscript) and isinstance(target.slice, ast.Constant) and isinstance(target.slice.value, str):
            return target.slice.value
        return ''

    def redact_value(value):
        # Calls such as getenv("KEY_NAME") refer to credentials without containing their values.
        if isinstance(value, (ast.Call, ast.Name, ast.Attribute)):
            return
        replace_literal(value, True)
        for child in ast.iter_child_nodes(value):
            redact_value(child)

    for node in ast.walk(tree):
        replace_literal(node)
        value, names = None, []
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            value = node.value
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            names = [target_name(target) for target in targets]
        elif isinstance(node, ast.keyword):
            value, names = node.value, [node.arg or '']
        if value is not None and any(sensitive(name) for name in names):
            redact_value(value)
        if isinstance(node, ast.Dict):
            for key, value in zip(node.keys, node.values):
                if isinstance(key, ast.Constant) and isinstance(key.value, str) and sensitive(key.value):
                    redact_value(value)
        if isinstance(node, ast.arguments):
            positional = [*node.posonlyargs, *node.args]
            defaults = list(zip(positional[-len(node.defaults):], node.defaults)) if node.defaults else []
            for arg, value in [*defaults, *zip(node.kwonlyargs, node.kw_defaults)]:
                if sensitive(arg.arg):
                    if value is not None:
                        redact_value(value)
    text_lines = text.splitlines(keepends=True)
    for token in tokenize.generate_tokens(io.StringIO(text).readline):
        if token.type == tokenize.COMMENT:
            clean = _text(token.string)
            if clean != token.string:
                begin = offsets[token.start[0] - 1] + len(text_lines[token.start[0] - 1][:token.start[1]].encode())
                end = offsets[token.end[0] - 1] + len(text_lines[token.end[0] - 1][:token.end[1]].encode())
                replacements[(begin, end)] = clean.encode()
    encoded = text.encode()
    for (begin, end), replacement in sorted(replacements.items(), reverse=True):
        encoded = encoded[:begin] + replacement + encoded[end:]
    clean = encoded.decode()
    ast.parse(clean)  # Refuse a broken projection instead of representing it as current source.
    return clean


def _write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def _time(value: Any, zone: Any) -> datetime | None:
    try:
        dt = datetime.fromisoformat(value.replace('Z', '+00:00'))
        return dt.replace(tzinfo=zone) if dt.tzinfo is None else dt.astimezone(zone)
    except (ValueError, AttributeError, TypeError):
        return None


def _git(source: Path, *args: str) -> dict:
    try:
        env = dict(os.environ, GIT_OPTIONAL_LOCKS='0', GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
        result = subprocess.run(['git', '-C', str(source), *args], capture_output=True,
                                text=True, errors='replace', timeout=60, env=env)
        return {'status': 'ok' if result.returncode == 0 else 'error',
                'stdout': result.stdout, 'error': _text(result.stderr), 'returncode': result.returncode}
    except (OSError, subprocess.SubprocessError) as exc:
        return {'status': 'error', 'stdout': '', 'error': type(exc).__name__}


def _excluded(path: Path) -> str | None:
    for part in path.parts:
        name = part.lower()
        if name in EXCLUDED_DIRS or name.endswith('.egg-info'):
            return 'runtime/build/cache directory'
        if (name == '.env' or name.startswith('.env.') or name.endswith(('.pem', '.key', '.p12', '.pfx', '.sqlite', '.db'))
                or name in {'.npmrc', '.netrc', '.pypirc', 'credentials', 'credentials.json', 'secrets.json', 'secrets.yaml', 'secrets.yml'}
                or name.startswith(('id_rsa', 'id_ed25519'))
                or re.fullmatch(r'(?:secrets?|credentials?|tokens?)\.(?:json|ya?ml|toml|ini|conf|txt)', name)):
            return 'credential/environment/runtime file'
        if 'trace' in name and name.endswith(('.json', '.jsonl')):
            return 'private trace'
    return None


class Collector:
    def __init__(self, anchor: Path, source: Path, output: Path, mount_origins: dict | None = None):
        self.anchor, self.source, self.output = anchor.resolve(), source.resolve(), output.resolve()
        self.errors: list[dict] = []
        self.omissions: list[dict] = []
        self.aliases = sorted(((Path(origin), {'source': self.source, 'anchor': self.anchor}[name])
                              for name, origin in (mount_origins or {}).items()
                              if name in ('source', 'anchor') and isinstance(origin, str)
                              and Path(origin).is_absolute()), key=lambda pair: len(pair[0].parts), reverse=True)

    def local(self, path: Path) -> Path:
        """Translate operator-granted host prefixes into existing sandbox mounts, never new grants."""
        for origin, mount in self.aliases:
            if path.is_relative_to(origin):
                return mount / path.relative_to(origin)
        return path

    def allowed(self, path: Path) -> bool:
        try:
            resolved = path.resolve()
            return any(resolved.is_relative_to(root) for root in (self.source, self.anchor))
        except (OSError, RuntimeError):
            return False

    def read_json(self, path: Path, *, optional: bool = False) -> dict | None:
        if not self.allowed(path) or self.has_symlink(path, self.anchor):
            self.omissions.append({'path': str(path), 'reason': 'symlink or external path'})
            return None
        if optional and not path.exists():
            return None
        try:
            value = json.loads(path.read_text(encoding='utf-8'))
            if not isinstance(value, dict):
                raise ValueError('expected object')
            return value
        except (OSError, ValueError) as exc:
            self.errors.append({'path': str(path), 'error': type(exc).__name__})
            return None

    @staticmethod
    def has_symlink(path: Path, root: Path) -> bool:
        current = path
        while current != root and current != current.parent:
            if current.is_symlink():
                return True
            current = current.parent
        return False

    def walk(self, root: Path, *, follow: bool = False):
        def visit(path: Path, relative: Path, ancestors: frozenset[Path]):
            if path.is_symlink():
                target = self.local(path.resolve())
                if not follow or not self.allowed(target):
                    yield path, relative, 'external_grant_required' if follow else 'symlink_skipped'
                    return
                path = target
            if not self.allowed(path):
                yield path, relative, 'external_grant_required'
                return
            reason = _excluded(relative)
            # Also enforce exclusions against the actual symlink target.
            resolved = path.resolve()
            roots = sorted((self.source, self.anchor), key=lambda root: len(root.parts), reverse=True)
            target_relative = next((resolved.relative_to(r) for r in roots if resolved.is_relative_to(r)), relative)
            if reason or _excluded(target_relative):
                yield path, relative, 'excluded'
                return
            if resolved == self.output or resolved.is_relative_to(self.output):
                yield path, relative, 'output_excluded'
                return
            if path.is_dir():
                if resolved in ancestors:
                    yield path, relative, 'symlink_cycle'
                    return
                try:
                    for child in sorted(path.iterdir()):
                        yield from visit(child, relative / child.name, ancestors | {resolved})
                except OSError as exc:
                    self.errors.append({'path': str(path), 'error': type(exc).__name__})
            else:
                yield path, relative, None
        yield from visit(root, Path('.'), frozenset())

    def snapshot(self, path: Path, relative: Path, prefix: str, status: str | None = None) -> dict:
        entry = {'path': relative.as_posix(), 'snapshot': None, 'sha256': None, 'bytes': None, 'status': status or 'pending'}
        if status:
            self.omissions.append({'path': str(path), 'reason': status})
            return entry
        try:
            data = path.read_bytes()
            entry.update(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
            if b'\0' in data:
                entry['status'] = 'binary'
                self.omissions.append({'path': str(path), 'reason': 'binary'})
                return entry
            text = data.decode('utf-8')
            try:
                value = json.loads(text)
            except ValueError:
                clean = _python_text(text) if path.suffix == '.py' else _text(text)
            else:
                clean = json.dumps(_redact(value), ensure_ascii=False, indent=2) + '\n'
            target = self.output / prefix / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(clean, encoding='utf-8')
            entry.update(status='snapshotted', snapshot=target.relative_to(self.output).as_posix(),
                         snapshot_sha256=hashlib.sha256(clean.encode()).hexdigest(), redacted=clean != text)
        except UnicodeDecodeError:
            entry['status'] = 'binary'
            self.omissions.append({'path': str(path), 'reason': 'non-UTF8/binary'})
        except (OSError, SyntaxError) as exc:
            entry.update(status='error', error=type(exc).__name__)
            self.errors.append({'path': str(path), 'error': type(exc).__name__})
        return entry

    def source_inventory(self) -> tuple[list[dict], str]:
        result = _git(self.source, 'ls-files', '-z', '--cached', '--others', '--exclude-standard')
        if result['status'] == 'ok':
            entries = []
            for name in sorted(set(result['stdout'].split('\0')) - {''}):
                relative = Path(name)
                path = self.source / relative
                status = ('excluded' if _excluded(relative) else
                          'symlink_skipped' if self.has_symlink(path, self.source) else
                          'external_grant_required' if not self.allowed(path) else
                          'output_excluded' if path.resolve().is_relative_to(self.output) else None)
                entries.append(self.snapshot(path, relative, 'source_snapshot', status))
            return entries, 'git tracked and nonignored untracked files'
        self.omissions.append({'path': str(self.source), 'reason': 'git inventory unavailable; filesystem fallback', 'detail': result['error']})
        return [self.snapshot(p, rel, 'source_snapshot', status) for p, rel, status in self.walk(self.source)], 'filesystem fallback'

    def plugins(self) -> dict:
        entries = []
        for kind in ('plugins', 'tools'):
            root = self.anchor / 'library' / kind
            if root.exists() or root.is_symlink():
                entries.extend(self.snapshot(p, Path(kind) / rel, 'plugin_snapshot', status)
                               for p, rel, status in self.walk(root, follow=True))
        references = []
        for entry in list(entries):
            if Path(entry['path']).name != 'tool.json' or not entry['snapshot']:
                continue
            try:
                spec = json.loads((self.output / entry['snapshot']).read_text(encoding='utf-8'))
            except (OSError, ValueError):
                continue
            if not isinstance(spec, dict):
                continue
            base = self.anchor / 'library' / Path(entry['path']).parent
            declared = [('entrypoint', spec.get('entrypoint'))]
            declared.extend(('imports', value) for value in spec.get('imports', []) if isinstance(value, str))
            if spec.get('environment'):
                references.append({'manifest': entry['path'], 'kind': 'environment',
                                   'path': spec['environment'], 'status': 'environment_not_copied'})
            for index, (kind, value) in enumerate(declared):
                if not isinstance(value, str) or not value:
                    continue
                target = self.local(Path(value)) if Path(value).is_absolute() else base / value
                reference = {'manifest': entry['path'], 'kind': kind, 'path': str(target)}
                if not self.allowed(target):
                    reference['status'] = 'external_grant_required'
                    self.omissions.append({'path': str(target), 'reason': 'external_grant_required'})
                elif target.resolve().is_relative_to(base.resolve()):
                    reference['status'] = 'covered_by_bundle_inventory'
                else:
                    prefix = Path('referenced') / Path(entry['path']).parent.name / kind / str(index)
                    new_entries = []
                    for path, relative, status in self.walk(target, follow=True):
                        relative = Path(target.name) if relative == Path('.') and not target.is_dir() else relative
                        new_entries.append(self.snapshot(path, prefix / relative, 'plugin_snapshot', status))
                    entries.extend(new_entries)
                    reference.update(status='inventoried', entries=[e['path'] for e in new_entries])
                references.append(reference)
        return {'entries': entries, 'references': references, 'scope': 'installed library plugins and tools, including skill, MCP and channel implementation resources',
                'limitations': ['External targets require an additional explicit read grant.',
                                'Shared execution environments are not copied; declarations remain in tool manifests.']}


def _body_summary(value: Any) -> dict:
    serialized = json.dumps(value, ensure_ascii=False, sort_keys=True)
    return {'type': type(value).__name__, 'characters': len(serialized),
            'sha256': hashlib.sha256(serialized.encode()).hexdigest(), 'content_omitted': 'private task or submission body'}


def _node(item: dict) -> dict:
    keys = ('node_id', 'agent', 'tree', 'pass_number', 'files', 'submitted', 'exit_status', 'route', 'commit', 'inputs', 'status', 'error', 'reason')
    result = {k: _redact(item[k], k) for k in keys if k in item}
    if 'submission' in item:
        result['submission'] = _body_summary(item['submission'])
    return result


def _run(raw: dict, graph: str, run_id: str) -> dict:
    keys = ('status', 'started', 'updated', 'cursor', 'active', 'parallel', 'reason', 'error', 'attempts',
            'runs', 'pause', 'paused', 'passes', 'activations', 'last_seq', 'decided', 'executed',
            'skipped', 'seq', 'ceased', 'commits', 'inputs')
    result = {k: _redact(raw[k], k) for k in keys if k in raw}
    result.update(graph=graph, run=run_id)
    for key in ('nodes', 'history'):
        result[key] = {k: _node(v) for k, v in (raw.get(key) or {}).items() if isinstance(v, dict)}
    trigger = raw.get('trigger') or {}
    result['trigger'] = _redact({k: trigger[k] for k in ('source', 'graph', 'run', 'node', 'schedule', 'parent_run', 'parent_node') if k in trigger})
    result['input'] = {k: _body_summary(v) for k, v in (raw.get('input') or {}).items()} if isinstance(raw.get('input'), dict) else _body_summary(raw.get('input'))
    result['objective'] = _body_summary(raw.get('objective'))
    return result


def _collect_history(c: Collector, start: datetime, end: datetime) -> tuple[list, list, list, list]:  # noqa: C901
    runs, inventory, graphs, previous = [], [], [], []
    root = c.anchor / 'workspaces'
    if not root.is_dir() or root.is_symlink():
        return runs, inventory, graphs, previous
    for graph_dir in sorted(root.iterdir()):
        if not graph_dir.is_dir() or graph_dir.is_symlink():
            c.omissions.append({'path': str(graph_dir), 'reason': 'not a real graph directory'})
            continue
        definition = c.read_json(graph_dir / 'graph.json', optional=True)
        if definition is not None:
            graphs.append({'graph': graph_dir.name, 'path': str((graph_dir / 'graph.json').relative_to(c.anchor)),
                           'sha256': hashlib.sha256((graph_dir / 'graph.json').read_bytes()).hexdigest(), 'definition': _redact(definition)})
            grants = c.read_json(graph_dir / 'local-inputs.json', optional=True)
            graphs[-1]['local_input_grants'] = _redact(grants) if grants is not None else None
        run_root = graph_dir / 'runs'
        if not run_root.is_dir() or run_root.is_symlink():
            continue
        for run_dir in sorted(run_root.iterdir()):
            if not run_dir.is_dir() or run_dir.is_symlink():
                c.omissions.append({'path': str(run_dir), 'reason': 'not a real run directory'})
                continue
            raw = c.read_json(run_dir / 'run.json')
            identity = {'graph': graph_dir.name, 'run': run_dir.name, 'path': str((run_dir / 'run.json').relative_to(c.anchor))}
            if raw is None:
                inventory.append({**identity, 'collection_status': 'unreadable', 'selected': False})
                continue
            started, updated = (_time(raw.get(k), end.tzinfo) for k in ('started', 'updated'))
            terminal = raw.get('status') in {'finished', 'failed', 'stopped', 'cancelled', 'canceled', 'completed'}
            # Intersection with the observation window, including long-running and late-updated runs.
            selected = bool((started and started < end and (not terminal or not updated or updated >= start))
                            or (updated and start <= updated < end))
            inventory.append({**identity, **{k: raw.get(k) for k in ('status', 'started', 'updated')},
                              'selected': selected, 'collection_status': 'ok'})
            record = _run(raw, graph_dir.name, run_dir.name)
            record['evidence_path'] = identity['path']
            for name, key in (('graph.json', 'frozen_graph'), ('plugins.json', 'frozen_plugins')):
                frozen = c.read_json(run_dir / name, optional=True)
                record[key] = _redact(frozen) if frozen is not None else None
            record['frozen_plugins_limitations'] = 'Run bindings preserve resource identities/digests, not historical implementation bytes; plugin_snapshot contains current installed resources.'
            record['calls'] = []
            for call_path in sorted((run_dir / 'control/.graph-calls').glob('*/graph-call.json')):
                call = c.read_json(call_path)
                if call is not None:
                    record['calls'].append({key: _redact(call[key], key) for key in
                                            ('graph', 'run', 'mode', 'node', 'invocation', 'status') if key in call})
            projected = Path('run_snapshot') / graph_dir.name / (run_dir.name + '.json')
            _write_json(c.output / projected, record)
            inventory[-1]['snapshot'] = projected.as_posix()
            if selected:
                runs.append(record)
            publish = (raw.get('nodes') or {}).get('publish', {})
            if (raw.get('status') != 'finished' or (updated and updated >= end) or not publish.get('submitted')
                    or str(publish.get('exit_status', '')).lower() not in {'submitted', 'completed', 'success', 'succeeded', '0'}):
                continue
            # Only RSI artifacts with a structured ledger qualify, regardless of graph directory name.
            ledger_path = run_dir / 'publish' / 'evolution.json'
            commit = publish.get('commit', '')
            if (not isinstance(commit, str) or not re.fullmatch(r'[0-9a-f]{40,64}', commit)
                    or c.has_symlink(ledger_path, c.anchor)):
                c.errors.append({'path': str(ledger_path), 'error': 'published ledger lacks a safe commit identity'})
                continue
            blob = _git(run_dir / 'publish', 'show', f'{commit}:evolution.json')
            try:
                if blob['status'] != 'ok':
                    listing = _git(run_dir / 'publish', 'ls-tree', '--name-only', commit)
                    if listing['status'] == 'ok' and 'evolution.json' not in listing['stdout'].splitlines():
                        continue  # Ordinary publication nodes need not produce an RSI ledger.
                    raise ValueError('published ledger commit is unavailable')
                ledger = json.loads(blob['stdout'])
                if not isinstance(ledger, dict):
                    raise ValueError('published ledger must be an object')
            except ValueError as exc:
                c.errors.append({'path': str(ledger_path), 'error': str(exc)})
                continue
            item = {**identity, 'updated': raw.get('updated'), 'commit': publish.get('commit'), 'ledger': _redact(ledger),
                    'ledger_path': str(ledger_path.relative_to(c.anchor))}
            report = _git(run_dir / 'publish', 'show', f'{commit}:rsi-report.md')
            if report['status'] == 'ok':
                item['report'] = _text(report['stdout'])
            else:
                c.errors.append({'path': str(run_dir / 'publish/rsi-report.md'), 'error': 'published report commit is unavailable'})
            previous.append(item)
    return runs, inventory, graphs, previous


def _dependency(path: str) -> bool:
    name = Path(path).name.lower()
    return (name in {'pyproject.toml', 'package.json', 'package-lock.json', 'uv.lock', 'poetry.lock',
                     'pipfile', 'pipfile.lock', 'yarn.lock', 'pnpm-lock.yaml', 'cargo.toml', 'cargo.lock',
                     'go.mod', 'go.sum', 'setup.py', 'setup.cfg', '.python-version', 'dockerfile'}
            or name.startswith(('requirements', 'constraints')) and name.endswith(('.txt', '.in'))
            or name.endswith(('.service', '.container')))


def _schedules(c: Collector) -> dict:
    schedules = {'entries': [], 'limitation': 'Plan definitions and admitted Runs cannot prove reasons for missed triggers.'}
    schedule_path = c.anchor / 'state/schedules.json'
    if schedule_path.is_file() and not c.has_symlink(schedule_path, c.anchor):
        try:
            rows = json.loads(schedule_path.read_text(encoding='utf-8'))
            if not isinstance(rows, list):
                raise ValueError('schedule list expected')
            for row in rows:
                if isinstance(row, dict):
                    schedules['entries'].append({key: _redact(value, key) if key != 'input' else _body_summary(value)
                                                 for key, value in row.items()})
        except (OSError, ValueError) as exc:
            c.errors.append({'path': str(schedule_path), 'error': type(exc).__name__})
    else:
        schedules['limitation'] += ' Schedule store unavailable.'
    return schedules


def collect(anchor: Path, source: Path, output: Path, *, end: datetime, lookback_days: int,
            mount_origins: dict | None = None) -> dict[str, Any]:
    if end.tzinfo is None:
        raise ValueError('end must include a timezone')
    if lookback_days < 1:
        raise ValueError('lookback_days must be positive')
    c = Collector(anchor, source, output, mount_origins)
    c.output.mkdir(parents=True, exist_ok=True)
    start = end - timedelta(days=lookback_days)
    snapshot, mode = c.source_inventory()
    plugins = c.plugins()
    runs, history, graphs, previous = _collect_history(c, start, end)
    schedules = _schedules(c)
    status = _git(c.source, 'status', '--porcelain=v1', '-z', '--untracked-files=all')
    changed = _git(c.source, 'diff', '--name-only', '-z', 'HEAD')
    status_paths = []
    status_fields = iter(status['stdout'].split('\0'))
    for field in status_fields:
        if len(field) < 4:
            continue
        status_paths.append(field[3:])
        if 'R' in field[:2] or 'C' in field[:2]:
            status_paths.append(next(status_fields, ''))
    changed_paths = sorted((set(changed['stdout'].split('\0')) | set(status_paths)) - {''})
    # Porcelain -z rename entries have an extra path; retain both paths in the raw record.
    untracked = [line[3:] for line in status['stdout'].split('\0') if line.startswith('?? ')]
    remote_result = _git(c.source, 'remote', '-v')
    remotes = sorted(set(re.findall(r'(?:https://github\.com/|git@github\.com:)([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)', remote_result['stdout'])))
    public_remotes = ['https://github.com/' + item.removesuffix('.git') for item in remotes]
    changes = {'public_upstream_urls': public_remotes, 'git_status': _text(status['stdout']), 'status_result': status['status'],
               'git_log': _redact(_git(c.source, 'log', f'--since={start.isoformat()}', f'--until={end.isoformat()}', '--date=iso-strict', '--format=%H%x09%ad%x09%s', '--name-status')),
               'changed_paths': changed_paths, 'untracked_paths': untracked,
               'git_diff_stat': _redact(_git(c.source, 'diff', '--stat', 'HEAD')),
               'limitations': 'No raw patch is exported: it may include deleted credentials and private files. Full eligible current files are in source_snapshot.'}
    declarations = [item for item in snapshot if _dependency(item['path'])]
    distributions = sorted(({'name': dist.metadata.get('Name', ''), 'version': dist.version}
                            for dist in importlib.metadata.distributions()), key=lambda d: d['name'].lower())
    environment = {'python': {'executable': sys.executable, 'version': platform.python_version(), 'packages': distributions},
                   'provenance': 'collector process in an Op sandbox; not verified service environment',
                   'git_remotes': public_remotes, 'collector_environment': {'python': sys.version, 'executable': sys.executable,
                   'platform': platform.platform(), 'distributions': distributions},
                   'declaration_files': declarations, 'service_environment': {'status': 'unverified',
                   'limitations': 'Collector runs in its own Op sandbox. Its interpreter and distributions do not establish the running service environment; manifests describe declared dependencies only.'}}
    index = {'start': start.isoformat(), 'end_exclusive': end.isoformat(), 'lookback_days': lookback_days,
             'run_count': len(runs), 'history_count': len(history), 'graph_count': len(graphs),
             'snapshot_count': sum(e['status'] == 'snapshotted' for e in snapshot),
             'source_inventory_mode': mode, 'caps': None,
             'limitations': ['Mechanical Run records do not prove provider acceptance or external side effects.',
                             'Private task bodies, conversation stores, traces and model reasoning are omitted.',
                             'Collection is read-only but not an atomic service snapshot; concurrent updates may occur.']}
    domains = {
        'runs': {'scope': 'scheduler, concurrency, failures, attempts and committed node/input references', 'priority': 'selected window; consult full history for recurrence', 'sources': ['runs.json', 'run-index.json', 'schedules.json']},
        'code': {'scope': 'all eligible source and tests; inspect changed paths then relevant implementation', 'priority': 'current changed files and evidence-linked modules', 'sources': ['source-index.json', 'changes.json'], 'paths': changed_paths + untracked},
        'graphs': {'scope': 'all deployed definitions and selected run frozen definitions', 'priority': 'graphs implicated by run evidence', 'sources': ['graphs.json', 'runs.json'], 'graphs': [g['graph'] for g in graphs]},
        'plugins': {'scope': 'installed skills, tools, MCP and channel implementations', 'priority': 'plugins bound to affected graphs and runs', 'sources': ['plugins.json', 'runs.json']},
        'dependencies': {'scope': 'dynamic dependency declarations and collector environment provenance', 'priority': 'declared constraints; service environment remains unverified', 'sources': ['environment.json', 'source-index.json'], 'paths': [d['snapshot'] for d in declarations if d['snapshot']]},
    }
    for name, value in [('index', index), ('runs', runs), ('run-index', _redact(history)), ('graphs', graphs), ('schedules', schedules),
                        ('source-index', snapshot), ('plugins', plugins), ('environment', environment),
                        ('changes', changes), ('previous', previous)]:
        _write_json(c.output / f'{name}.json', value)
    prior_ids = {}
    for report in sorted(previous, key=lambda item: item.get('updated') or ''):
        for entry in [*report['ledger'].get('proposals', []), *report['ledger'].get('carry_forward', [])]:
            if isinstance(entry, dict) and isinstance(entry.get('id'), str):
                prior_ids[entry['id']] = {'id': entry['id'], 'status': entry.get('status'),
                                         'graph': report['graph'], 'run': report['run'],
                                         'evidence': 'previous.json', 'locator': f'run={report["run"]}; id={entry["id"]}'}
    _write_json(c.output / 'previous-index.json', {'proposals': list(prior_ids.values()),
                                                'report_count': len(previous), 'details': 'previous.json'})
    for domain, value in domains.items():
        _write_json(c.output / 'domains' / f'{domain}.json', value)
    sections = [f"## {p['graph']} / {p['run']}\n\n{p.get('report', '')}\n\n### Evolution ledger\n\n```json\n{json.dumps(p['ledger'], ensure_ascii=False, indent=2)}\n```" for p in previous]
    (c.output / 'previous.md').write_text('\n\n'.join(sections) or 'No finished, successfully published RSI report is available.\n', encoding='utf-8')
    summary = {'start': start.isoformat(), 'end_exclusive': end.isoformat(), 'runs': len(runs), 'history': len(history),
               'graphs': len(graphs), 'previous_sections': len(previous), 'caps': None,
               'source_statuses': dict(Counter(e['status'] for e in snapshot)),
               'plugin_statuses': dict(Counter(e['status'] for e in plugins['entries'])),
               'errors': c.errors, 'omissions': c.omissions, 'privacy_omissions': index['limitations'][1]}
    _write_json(c.output / 'collection.json', summary)
    return summary


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--anchor', type=Path, default=Path('/local-inputs/anchor'))
    parser.add_argument('--source', type=Path, default=Path('/local-inputs/source'))
    parser.add_argument('--output', type=Path, default=Path('evidence'))
    parser.add_argument('--end', help='ISO datetime with timezone; defaults to ANCHOR_INPUT or now')
    parser.add_argument('--lookback-days', type=int, default=7)
    parser.add_argument('--grants', type=Path, default=Path('/local-inputs/grants'))
    args = parser.parse_args()
    raw_input = json.loads(os.environ.get('ANCHOR_INPUT', '{}'))
    cutoff = args.end or raw_input.get('end')
    end = datetime.fromisoformat(cutoff) if cutoff else datetime.now(ZoneInfo(raw_input.get('timezone', DEFAULT_ZONE)))
    days = raw_input.get('lookback_days', args.lookback_days)
    if end.tzinfo is None or type(days) is not int or days < 1:
        parser.error('end must include a timezone and lookback-days must be positive')
    origins = {}
    if args.grants.is_file():
        granted = json.loads(args.grants.read_text(encoding='utf-8')).get('collect', {})
        origins = {name: granted[name] for name in ('source', 'anchor') if name in granted}
    print(json.dumps(collect(args.anchor, args.source, args.output, end=end,
                             lookback_days=days, mount_origins=origins), ensure_ascii=False))


if __name__ == '__main__':
    main()
