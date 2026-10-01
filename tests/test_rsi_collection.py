from datetime import datetime
import importlib.util
import json
from pathlib import Path
import subprocess

import pytest

SPEC = importlib.util.spec_from_file_location('rsi_collect', Path(__file__).parents[1] / 'scripts/rsi/collect.py')
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)
END = datetime.fromisoformat('2026-10-01T00:00:00+08:00')


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value) if not isinstance(value, str) else value)
    return path


def read(path):
    return json.loads(path.read_text())


def git(root, *args):
    return subprocess.run(['git', '-C', str(root), *args], check=True, capture_output=True, text=True).stdout


def setup(tmp_path, *, use_git=True):
    source, anchor, out = (tmp_path / n for n in ('source', 'anchor', 'output'))
    source.mkdir()
    anchor.mkdir()
    if use_git:
        git(source, 'init', '-q')
    return source, anchor, out


def collect(source, anchor, out):
    return collector.collect(anchor, source, out, end=END, lookback_days=7)


def run(anchor, number, **values):
    return write(anchor / 'workspaces/new-workflow/runs' / str(number) / 'run.json', {
        'started': '2026-09-28T00:00:00+08:00', 'updated': '2026-09-28T01:00:00+08:00',
        'status': 'finished', 'nodes': {}, **values})


def test_dynamic_complete_source_graph_and_installed_plugin(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(source / 'new-module/deep/added.py', 'secret = os.getenv("API_SECRET")\n' + 'print("logic")\n' * 2500)
    write(source / 'new-app/package.json', {'dependencies': {'fresh-package': '^9.1'}})
    write(source / 'tracked.py', 'print("tracked")')
    write(source / '.gitignore', 'ignored/\n')
    write(source / 'ignored/skip.py', 'ignored')
    git(source, 'add', 'tracked.py')
    definition = {'nodes': [{'id': f'n-{i}', 'with': 'full instructions'} for i in range(125)], 'agents': {'x': {'system': 'full system'}}}
    write(anchor / 'workspaces/new-workflow/graph.json', definition)
    bundle = source / 'plugins/new-plugin'
    write(bundle / 'plugin.json', {'name': 'dynamic', 'skills': ['skills/new'], 'mcpServers': {'x': {'command': 'python', 'args': ['server.py']}}})
    write(bundle / 'skills/new/SKILL.md', 'Skill instruction')
    write(bundle / 'server.py', 'print("mcp implementation")')
    write(bundle / 'channel.json', {'entrypoint': 'channel.py'})
    write(bundle / 'channel.py', 'print("channel implementation")')
    installed = anchor / 'library/plugins/new-plugin'
    installed.parent.mkdir(parents=True)
    installed.symlink_to(bundle, target_is_directory=True)
    write(anchor / 'library/tools/new-tool/tool.json', {'entrypoint': 'run.py'})
    write(anchor / 'library/tools/new-tool/run.py', 'print("tool implementation")')
    result = collect(source, anchor, out)
    assert result['graphs'] == 1
    assert read(out / 'graphs.json')[0]['definition'] == definition
    inventory = {p['path']: p for p in read(out / 'source-index.json')}
    assert 'ignored/skip.py' not in inventory
    assert (out / inventory['new-module/deep/added.py']['snapshot']).read_text() == (source / 'new-module/deep/added.py').read_text()
    plugins = {p['path']: p for p in read(out / 'plugins.json')['entries']}
    for path in ('plugins/new-plugin/skills/new/SKILL.md', 'plugins/new-plugin/server.py', 'plugins/new-plugin/channel.py', 'tools/new-tool/run.py'):
        assert plugins[path]['status'] == 'snapshotted'
    dependencies = read(out / 'domains/dependencies.json')
    assert 'source_snapshot/new-app/package.json' in dependencies['paths']
    environment = read(out / 'environment.json')
    assert environment['service_environment']['status'] == 'unverified'
    assert 'collector' in environment['provenance']


def test_operator_mount_aliases_resolve_installed_host_symlinks_without_expanding_access(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(source / 'plugins/fresh/plugin.json', {'name': 'fresh'})
    write(source / 'plugins/fresh/skills/a/SKILL.md', 'new skill')
    installed = anchor / 'library/plugins/fresh'
    installed.parent.mkdir(parents=True)
    installed.symlink_to('/operator/project/plugins/fresh')
    external = anchor / 'library/plugins/private'
    external.symlink_to('/not-granted/private')
    collector.collect(anchor, source, out, end=END, lookback_days=7,
                      mount_origins={'source': '/operator/project', 'anchor': '/operator/project/.local/demo'})
    entries = {item['path']: item for item in read(out / 'plugins.json')['entries']}
    assert entries['plugins/fresh/skills/a/SKILL.md']['status'] == 'snapshotted'
    assert entries['plugins/private']['status'] == 'external_grant_required'


def test_old_history_can_be_inspected_via_its_projected_snapshot(tmp_path):
    source, anchor, out = setup(tmp_path)
    run(anchor, 'old', started='2026-08-01T00:00:00+08:00', updated='2026-08-02T00:00:00+08:00',
        status='failed', error='interface mismatch')
    collect(source, anchor, out)
    assert read(out / 'runs.json') == []
    old = read(out / 'run-index.json')[0]
    assert read(out / old['snapshot'])['error'] == 'interface mismatch'


def test_nested_authorized_anchor_root_and_unquoted_credentials(tmp_path):
    source = tmp_path / 'source'
    source.mkdir()
    anchor = source / '.local/demo'
    write(anchor / 'library/plugins/fresh/plugin.json', {'name': 'fresh'})
    write(source / 'notes.md', 'Authorization: Bearer abcdefgh1234\nAPI_KEY=abcdefgh9876\nsecret = os.getenv("API_SECRET")')
    out = tmp_path / 'out'
    collector.collect(anchor, source, out, end=END, lookback_days=7)
    entries = read(out / 'plugins.json')['entries']
    assert any(item['status'] == 'snapshotted' for item in entries)
    content = (out / 'source_snapshot/notes.md').read_text()
    assert 'abcdefgh1234' not in content and 'abcdefgh9876' not in content
    assert 'os.getenv("API_SECRET")' in content


def test_camelcase_and_multiline_literal_credentials_are_redacted(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(source / 'config.json', {'apiKey': 'fixture-camel-key', 'accessToken': 'fixture-access',
                                 'clientSecret': 'fixture-client', 'privateKey': 'fixture-private',
                                 'bearer_token_env_var': 'SERVICE_TOKEN'})
    write(source / 'settings.py', 'client_secret = """fixture-multiline\nsecond-sensitive-line"""\nprint("kept")')
    write(source / 'settings.yaml', 'api_key: |\n  fixture-yaml\n  another-sensitive-line\nname: kept\n')
    collect(source, anchor, out)
    for path in (out / 'source_snapshot').rglob('*'):
        if path.is_file():
            text = path.read_text()
            assert 'fixture-' not in text and 'sensitive-line' not in text
    assert read(out / 'source_snapshot/config.json')['bearer_token_env_var'] == 'SERVICE_TOKEN'


def test_python_redaction_preserves_valid_syntax_types_and_following_lines():
    import ast
    text = '''import os
class Config:
    secret: str
    def read(self, secret: str):
        import os
        token = os.getenv("TOKEN")
        return token
clientSecret = "synthetic-sensitive-value"
def func(api_key: str = "synthetic-default"):
    return api_key
'''
    clean = collector._python_text(text)
    ast.parse(clean)
    assert 'secret: str' in clean and 'import os' in clean
    assert 'synthetic-sensitive-value' not in clean
    assert 'synthetic-default' not in clean


def test_python_credential_attributes_subscripts_bytes_concatenation_and_comments():
    import ast
    text = '''# Authorization: Bearer SYNTHETIC_COMMENT_CREDENTIAL
self.api_key = "SYNTHETIC_ATTRIBUTE_SECRET"
config["accessToken"] = "SYNTHETIC_SUBSCRIPT_SECRET"
password = b"SYNTHETIC_BYTES_SECRET"
api_key = "SYNTHETIC_" + "CONCAT_SECRET"
secret = os.getenv("KEY_NAME")
'''
    clean = collector._python_text(text)
    ast.parse(clean)
    assert 'SYNTHETIC_' not in clean and 'CONCAT_SECRET' not in clean
    assert 'os.getenv("KEY_NAME")' in clean


def test_schedule_and_call_provenance_omit_business_input(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(anchor / 'state/schedules.json', [{'id': 'weekly', 'graph': 'new-workflow', 'input': {'text': 'PRIVATE'},
                                           'rule': {'type': 'weekly', 'time': '09:00'}}])
    path = run(anchor, 'called')
    write(path.parent / 'control/.graph-calls/ref/graph-call.json', {
        'graph': 'child', 'run': 'child-run', 'mode': 'wait', 'input': {'private': 'PRIVATE'}})
    collect(source, anchor, out)
    assert read(out / 'runs.json')[0]['calls'] == [{'graph': 'child', 'run': 'child-run', 'mode': 'wait'}]
    assert 'PRIVATE' not in (out / 'schedules.json').read_text()


def test_all_history_and_window_overlap_without_caps_or_private_bodies(tmp_path):
    source, anchor, out = setup(tmp_path)
    for i in range(550):
        run(anchor, i, attempts={'node|1': 4}, runs={'node': 1}, active=['node'], parallel={'branch': 'running'},
            input={'message': 'PRIVATE USER MESSAGE'}, objective='PRIVATE USER OBJECTIVE',
            nodes={'node': {'submission': 'PRIVATE ANSWER', 'commit': 'abc', 'inputs': [['earlier', 'def']], 'submitted': True, 'files': [str(k) for k in range(150)]}})
    run(anchor, 'late', started='2026-08-01T00:00:00+08:00', updated='2026-09-30T00:00:00+08:00')
    run(anchor, 'active-old', started='2026-08-01T00:00:00+08:00', updated='2026-08-01T00:00:00+08:00', status='running')
    run(anchor, 'outside', started='2026-08-01T00:00:00+08:00', updated='2026-08-02T00:00:00+08:00')
    run(anchor, 'future', started='2026-10-02T00:00:00+08:00', updated='2026-10-03T00:00:00+08:00')
    frozen = {'nodes': [{'id': 'historical'}]}
    write(anchor / 'workspaces/new-workflow/runs/0/graph.json', frozen)
    write(anchor / 'workspaces/new-workflow/runs/0/plugins.json', {'node': [{'id': 'old', 'digest': '123'}]})
    collect(source, anchor, out)
    records = read(out / 'runs.json')
    assert len(records) == 552
    assert len(read(out / 'run-index.json')) == 554
    first = next(r for r in records if r['run'] == '0')
    assert first['frozen_graph'] == frozen
    assert first['attempts'] == {'node|1': 4}
    assert first['active'] == ['node']
    assert first['nodes']['node']['inputs'] == [['earlier', 'def']]
    assert len(first['nodes']['node']['files']) == 150
    assert 'PRIVATE' not in (out / 'runs.json').read_text()


def test_secret_exclusions_redaction_and_symlink_escape(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(source / '.env', 'PASSWORD=TRACKED_SECRET')
    write(source / 'credentials.json', {'key': 'CREDENTIAL_SECRET'})
    git(source, 'add', '.env', 'credentials.json')
    write(source / 'valid.py', 'api_key = "LITERAL_SECRET"\nsecret = os.getenv("SERVICE_SECRET")\nsecret = config.secret\nprint("AFTER SECRET")\n')
    write(source / 'valid.json', {'api_key': 'JSON_SECRET', 'secret_env_var': 'SERVICE_SECRET', 'password': '${PASS_ENV}', 'nested': {'Authorization': 'Bearer AUTH_SECRET'}})
    outside = write(tmp_path / 'outside.txt', 'OUTSIDE_SECRET')
    (source / 'escape.txt').symlink_to(outside)
    (source / 'internal.txt').symlink_to(source / 'valid.py')
    external = anchor / 'library/plugins/external'
    external.parent.mkdir(parents=True)
    external.symlink_to(tmp_path / 'external-bundle', target_is_directory=True)
    write(tmp_path / 'external-bundle/plugin.json', {'description': 'EXTERNAL_SECRET'})
    write(source / 'binary.dat', b'not used'.decode() + '\0')
    result = collect(source, anchor, out)
    inventory = {v['path']: v for v in read(out / 'source-index.json')}
    assert inventory['.env']['status'] == 'excluded'
    assert inventory['credentials.json']['status'] == 'excluded'
    assert inventory['escape.txt']['status'] == 'symlink_skipped'
    assert inventory['internal.txt']['status'] == 'symlink_skipped'
    assert inventory['binary.dat']['status'] == 'binary'
    assert any(e['status'] == 'external_grant_required' for e in read(out / 'plugins.json')['entries'])
    evidence = '\n'.join(p.read_text() for p in out.rglob('*') if p.is_file())
    for secret in ('TRACKED_SECRET', 'CREDENTIAL_SECRET', 'LITERAL_SECRET', 'JSON_SECRET', 'AUTH_SECRET', 'OUTSIDE_SECRET', 'EXTERNAL_SECRET'):
        assert secret not in evidence
    code = (out / 'source_snapshot/valid.py').read_text()
    assert 'os.getenv("SERVICE_SECRET")' in code and 'secret = config.secret' in code
    assert 'AFTER SECRET' in code
    config = read(out / 'source_snapshot/valid.json')
    assert config['secret_env_var'] == 'SERVICE_SECRET'
    assert config['password'] == '${PASS_ENV}'
    assert result['omissions']


def test_previous_full_ledger_requires_finished_and_successful_publish(tmp_path):
    source, anchor, out = setup(tmp_path)
    ledger = {'proposals': [{'id': i, 'details': 'evidence ' * 100} for i in range(140)]}
    for name, status, submitted, exit_status in (
        ('good', 'finished', True, 'Submitted'), ('running', 'running', True, 'Submitted'),
        ('failed', 'failed', True, 'Submitted'), ('not-submitted', 'finished', False, 'Submitted'),
        ('publish-failed', 'finished', True, 'Failed')):
        path = run(anchor, name, status=status, nodes={'publish': {'submitted': submitted, 'exit_status': exit_status, 'commit': 'abc'}})
        write(path.parent / 'publish/evolution.json', ledger)
        write(path.parent / 'publish/rsi-report.md', 'Report ' * 3000)
        git(path.parent / 'publish', 'init', '-q')
        git(path.parent / 'publish', 'add', '.')
        git(path.parent / 'publish', '-c', 'user.name=test', '-c', 'user.email=test@test', 'commit', '-qm', 'published')
        raw = read(path)
        raw['nodes']['publish']['commit'] = git(path.parent / 'publish', 'rev-parse', 'HEAD').strip()
        write(path, raw)
        write(path.parent / 'publish/evolution.json', {'proposals': [{'id': 'UNCOMMITTED'}]})
    collect(source, anchor, out)
    previous = read(out / 'previous.json')
    assert [p['run'] for p in previous] == ['good']
    assert previous[0]['ledger'] == ledger
    assert len(previous[0]['report']) == 21000
    markdown = (out / 'previous.md').read_text()
    rendered_ledger = markdown.split('```json\n')[1].split('\n```')[0]
    assert json.loads(rendered_ledger) == ledger
    (anchor / 'workspaces/new-workflow/runs/good/publish/evolution.json').unlink()
    collect(source, anchor, out)
    assert read(out / 'previous.json')[0]['ledger'] == ledger


def test_filesystem_fallback_exclusions_and_errors(tmp_path):
    source, anchor, out = setup(tmp_path, use_git=False)
    write(source / 'fresh/module.py', 'pass\n')
    write(source / 'node_modules/pkg/index.js', 'DO_NOT_COPY')
    write(source / '.env.production', 'SECRET=DO_NOT_COPY')
    write(anchor / 'workspaces/new-workflow/runs/bad/run.json', '{bad')
    summary = collect(source, anchor, out)
    entries = {e['path']: e for e in read(out / 'source-index.json')}
    assert entries['fresh/module.py']['status'] == 'snapshotted'
    assert entries['node_modules']['status'] == 'excluded'
    assert entries['.env.production']['status'] == 'excluded'
    assert summary['errors']
    assert read(out / 'run-index.json')[0]['collection_status'] == 'unreadable'
    assert read(out / 'index.json')['source_inventory_mode'] == 'filesystem fallback'


def test_invalid_window_fails_explicitly(tmp_path):
    with pytest.raises(ValueError):
        collector.collect(tmp_path, tmp_path, tmp_path / 'out', end=datetime(2026, 1, 1), lookback_days=7)


def test_tool_references_authorized_resources_and_external_grants(tmp_path):
    source, anchor, out = setup(tmp_path)
    implementation = write(anchor / 'extensions/run.py', 'print("implementation outside bundle")')
    write(anchor / 'library/tools/local/tool.json', {'entrypoint': str(implementation), 'imports': [str(tmp_path / 'ungranted')]})
    collect(source, anchor, out)
    plugins = read(out / 'plugins.json')
    assert any(e['snapshot'] and 'referenced/local/entrypoint' in e['snapshot'] for e in plugins['entries'])
    assert any(r['kind'] == 'imports' and r['status'] == 'external_grant_required' for r in plugins['references'])


def test_real_changes_include_unborn_staged_and_public_remote(tmp_path):
    source, anchor, out = setup(tmp_path)
    write(source / 'new.py', 'print("new")')
    git(source, 'add', 'new.py')
    git(source, 'remote', 'add', 'origin', 'git@github.com:some-owner/any-repo.git')
    collect(source, anchor, out)
    assert 'new.py' in read(out / 'changes.json')['changed_paths']
    assert read(out / 'environment.json')['git_remotes'] == ['https://github.com/some-owner/any-repo']
