"""RSI research inventory, public transport boundaries and evidence integrity."""

import importlib.util
import json
from http.client import IncompleteRead
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request

import pytest

SPEC = importlib.util.spec_from_file_location("rsi_research", Path(__file__).parents[1] / "scripts/rsi/research.py")
research = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(research)


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data) if isinstance(data, (list, dict)) else data)


def test_partial_http_response_is_evidence_failure_not_worker_crash(monkeypatch):
    class Response:
        headers = {}
        status = 200

        def __enter__(self):
            return self

        def __exit__(self, *_):
            return False

        def geturl(self):
            return 'https://api.github.com/repos/vendor/project/releases'

        def read(self, _):
            raise IncompleteRead(b'{"unfinished":', 20)

    class Opener:
        def open(self, *args, **kwargs):
            return Response()

    monkeypatch.setattr(research, 'build_opener', lambda *_: Opener())
    payload, headers = research._get('https://api.github.com/repos/vendor/project/releases')
    assert 'IncompleteRead' in payload['error']
    assert headers['http_status'] == 200
    page = research._pages('https://api.github.com/repos/vendor/project/releases', None, 'published_at')
    assert page['status'] == 'partial'
    assert page['items'] == [] and 'IncompleteRead' in page['errors'][0]['error']


def test_requirements_and_unsupported_declarations_are_visible(tmp_path):
    write(tmp_path / 'new-plugin/requirements.txt', 'new-plugin-dependency>=1\n-r extras.txt\n')
    write(tmp_path / 'setup.cfg', '[options]\ninstall_requires=old-format-package\n')
    targets, manifests, notices = research._inventory(tmp_path)
    assert 'new-plugin-dependency' in targets['pypi']
    assert any(item['path'] == 'setup.cfg' and item['status'] == 'unsupported' for item in manifests)
    assert any('include' in item.get('limitation', '') for item in notices)


def fake_registry(endpoint):
    if "pypi.org" in endpoint:
        name = endpoint.split("/")[-2]
        return {"info": {"name": name, "version": "9.0", "project_urls": {"Source": f"https://github.com/vendor/{name}"}}}, {}
    if "npmjs.org" in endpoint:
        return {"name": "@scope/new-package", "version": "3.0", "repository": {"url": "git+https://github.com/vendor/node-new.git"}}, {}
    if "/releases?" in endpoint or "/issues?" in endpoint:
        return [], {}
    return {"name": endpoint.split("/")[-1]}, {}


def test_nested_declarations_extras_groups_installed_and_dynamic_repos(tmp_path, monkeypatch):
    source = tmp_path / "evidence/source_snapshot"
    write(source / "pyproject.toml", '''[project]
dependencies = ["base-package>=1; python_version >= '3.12'"]
[project.optional-dependencies]
channels = ["future-sdk[wire]==2"]
dev = ["test-tool~=4"]
[dependency-groups]
lint = ["lint-tool>=1", {include-group="tests"}]
tests = ["test-tool~=4"]
''')
    write(source / "plugins/new/pyproject.toml", '[project]\ndependencies = ["new-plugin-lib>=1"]\n')
    write(source / "ui/package.json", {"dependencies": {"@scope/new-package": "^2"}, "devDependencies": {"compiler-new": "~1"}})
    write(source / "ui/package-lock.json", {"packages": {"node_modules/@scope/new-package": {"version": "2.5"}}})
    write(source.parent / "environment.json", {"python": {"packages": [{"name": "future_sdk", "version": "2"}]}, "git_remotes": ["https://github.com/project/source.git"]})
    write(source.parent / "index.json", {"start": "2026-09-01T00:00:00Z", "end_exclusive": "2026-10-01T00:00:00Z"})
    monkeypatch.setattr(research, "_get", fake_registry)
    summary = research.research(source, tmp_path / "out", {"github_repos": ["extra/explicit"]})
    result = json.loads((tmp_path / "out/ecosystem.json").read_text())
    packages = {item["package"]: item for item in result["pypi"]}
    assert summary["pypi"] == 5
    assert packages["future-sdk"]["declarations"] == [{"manifest": "pyproject.toml", "group": "project.optional-dependencies.channels", "requirement": "future-sdk[wire]==2"}]
    assert packages["future-sdk"]["installed"][0]["version"] == "2"
    assert packages["new-plugin-lib"]["declarations"][0]["manifest"] == "plugins/new/pyproject.toml"
    assert {"vendor/future-sdk", "vendor/new-plugin-lib", "vendor/node-new", "project/source", "extra/explicit"} <= {r["repo"] for r in result["github"]}
    npm = next(p for p in result["npm"] if p["package"] == "@scope/new-package")
    assert npm["endpoint"].endswith("/%40scope%2Fnew-package/latest")
    assert npm["installed"][0]["version"] == "2.5"
    assert npm["installed"][0]["verified_runtime_install"] is False
    assert result["source"]["frozen_snapshot"] is True


@pytest.mark.parametrize("key,value", [("python_packages", "thing"), ("npm_packages", {}), ("github_repos", None),
    ("github_repos", ["owner/repo?bad"]), ("npm_packages", ["@scope/pkg/other"]), ("npm_packages", ["../evil"]),
    ("python_packages", [1]), ("python_packages", ["foo>=2"])])
def test_input_lists_strict(tmp_path, key, value):
    with pytest.raises(ValueError, match=key):
        research.research(tmp_path, tmp_path / "out", {key: value})


def test_packages_and_repos_not_silently_capped(tmp_path, monkeypatch):
    monkeypatch.setattr(research, "_get", fake_registry)
    research.research(tmp_path, tmp_path / "out", {"python_packages": [f"lib{i}" for i in range(37)]})
    result = json.loads((tmp_path / "out/ecosystem.json").read_text())
    assert len(result["pypi"]) == len(result["github"]) == 37


def test_registry_errors_survive_projection_and_sources_report_failure(tmp_path, monkeypatch):
    monkeypatch.setattr(research, "_get", lambda url: ({"error": "HTTPError: 429 Too Many Requests"}, {"http_status": 429}))
    summary = research.research(tmp_path, tmp_path / "out", {"python_packages": ["missing"], "npm_packages": ["missing"], "github_repos": ["vendor/missing"]})
    result = json.loads((tmp_path / "out/ecosystem.json").read_text())
    for group in ("pypi", "npm"):
        assert "429" in result[group][0]["error"]
        assert "429" in result[group][0]["data"]["error"]
        assert result[group][0]["repository_discovery"] == "registry_error"
    assert summary["errors"] == 3
    markdown = (tmp_path / "out/sources.md").read_text()
    assert markdown.count("partial/error:") == 3
    assert "429" in markdown


def test_redirect_denies_host_escape():
    req = Request("https://pypi.org/pypi/demo/json")
    with pytest.raises(ValueError, match="allowlist"):
        research._SafeRedirect().redirect_request(req, None, 302, "Found", {}, "https://internal.example/secrets")
    redirected = research._SafeRedirect().redirect_request(req, None, 302, "Found", {}, "https://pypi.org/redirect")
    assert redirected.full_url == "https://pypi.org/redirect"


@pytest.mark.parametrize("url", ["http://pypi.org/a", "https://pypi.org.evil/a", "https://user@pypi.org/a", "https://pypi.org:444/a", "https://pypi.org/a#secret"])
def test_url_allowlist(url):
    with pytest.raises(ValueError):
        research._url(url)


def test_window_pagination_release_bodies_and_issues_exclude_prs(monkeypatch):
    calls = []
    def get(url):
        calls.append(url)
        if "/releases?" in url:
            page2 = "page=2" in url
            return ([{"tag_name": "new" if page2 else "old", "body": "Changes: fixes wire protocol", "published_at": "2026-09-15T10:00:00Z" if page2 else "2026-08-01T00:00:00Z"}],
                    {} if page2 else {"link": '<https://api.github.com/repos/vendor/lib/releases?per_page=100&page=2>; rel="next"'})
        if "/issues?" in url:
            return [{"number": 1, "updated_at": "2026-09-02T00:00:00Z", "pull_request": {}},
                    {"number": 2, "updated_at": "2026-09-02T00:00:00Z", "body": "real issue"},
                    {"number": 3, "updated_at": "2026-10-01T00:00:00Z"}], {}
        return {"name": "lib"}, {}
    monkeypatch.setattr(research, "_get", get)
    result = research._repo("vendor/lib", {"start": "2026-09-01T00:00:00Z", "end_exclusive": "2026-10-01T00:00:00Z"})
    assert [r["tag_name"] for r in result["releases"]["items"]] == ["new"]
    assert result["releases"]["items"][0]["body"].startswith("Changes:")
    assert [i["number"] for i in result["recent_issues"]["items"]] == [2]
    assert any("since=" in url for url in calls)
    assert result["discussions"]["status"] == "not_collected"


def test_bad_pagination_is_preserved_as_failure(monkeypatch):
    monkeypatch.setattr(research, "_get", lambda url: ([], {"link": '<https://internal.example/a>; rel="next"'}))
    result = research._pages("https://api.github.com/repos/a/b/releases", {"start": "2026-09-01T00:00:00Z"}, "published_at")
    assert result["status"] == "partial"
    assert "allowlist" in result["errors"][0]["error"]


def test_transport_429_headers_and_bounded_body(monkeypatch):
    class Opener:
        def open(self, request, timeout):
            raise HTTPError(request.full_url, 429, "Too Many Requests", {"Retry-After": "30"}, None)
    monkeypatch.setattr(research, "build_opener", lambda handler: Opener())
    data, headers = research._get("https://pypi.org/pypi/foo/json")
    assert headers["http_status"] == 429
    assert headers["retry-after"] == "30"
    assert "429" in data["error"]


def test_missing_snapshot_and_parse_failures_are_visible(tmp_path, monkeypatch):
    source = tmp_path / "source_snapshot"
    write(source / "nested/pyproject.toml", "[project\ninvalid")
    monkeypatch.setattr(research, "_get", fake_registry)
    research.research(source, tmp_path / "out", {})
    result = json.loads((tmp_path / "out/ecosystem.json").read_text())
    assert result["coverage"]["manifests"][0]["status"] == "error"
    assert result["coverage"]["evidence_errors"]


def test_transport_response_size_limit_is_explicit(monkeypatch):
    class Response:
        headers = {}
        status = 200
        def __enter__(self):
            return self
        def __exit__(self, *args):
            return False
        def geturl(self):
            return "https://pypi.org/pypi/foo/json"
        def read(self, count):
            assert count == research.MAX_BYTES + 1
            return b"x" * count
    class Opener:
        def open(self, request, timeout):
            assert timeout == research.TIMEOUT_SECONDS
            return Response()
    monkeypatch.setattr(research, "build_opener", lambda handler: Opener())
    data, headers = research._get("https://pypi.org/pypi/foo/json")
    assert "transport byte limit" in data["error"]
    assert headers["http_status"] == 200


def test_malformed_activity_timestamp_is_an_evidence_error(monkeypatch):
    monkeypatch.setattr(research, "_get", lambda url: ([{"published_at": "invalid"}], {}))
    result = research._pages("https://api.github.com/repos/a/b/releases", {"start": "2026-09-01T00:00:00Z"}, "published_at")
    assert result["status"] == "partial"
    assert "invalid activity timestamp" in result["errors"][0]["error"]
