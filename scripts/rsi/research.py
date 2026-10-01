"""Public dependency signals from the collector's frozen source and evidence (stdlib only)."""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from http.client import HTTPException
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urljoin, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

ALLOWED_HOSTS = {"api.github.com", "pypi.org", "registry.npmjs.org"}
MAX_BYTES = 32_000_000
TIMEOUT_SECONDS = 25
TRANSPORT_WORKERS = 4
EXCLUDED_DIRS = {".git", ".venv", "venv", "node_modules", "__pycache__", "dist", "build"}
PYTHON_NAME = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9_.-]*[A-Za-z0-9])?")
NPM_NAME = re.compile(r"(?:@[a-z0-9][a-z0-9._-]*/)?[a-z0-9][a-z0-9._-]*")
REPO_NAME = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")


def _now() -> str:
    return datetime.now(timezone.utc).isoformat()


def _url(value: str) -> str:
    parsed = urlsplit(value)
    if (parsed.scheme != "https" or parsed.hostname not in ALLOWED_HOSTS
            or parsed.username or parsed.password or parsed.port not in (None, 443) or parsed.fragment):
        raise ValueError("research URL is outside the HTTPS public endpoint allowlist")
    return value


class _SafeRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return super().redirect_request(req, fp, code, msg, headers, _url(urljoin(req.full_url, newurl)))


def _get(endpoint: str) -> tuple[dict | list | None, dict]:
    headers: dict = {"retrieved_at": _now()}
    try:
        request = Request(_url(endpoint), headers={"Accept": "application/json", "User-Agent": "Anchor-RSI/2.0"})
        with build_opener(_SafeRedirect()).open(request, timeout=TIMEOUT_SECONDS) as response:
            _url(response.geturl())
            headers.update({key.lower(): value for key, value in response.headers.items()
                            if key.lower() in {"etag", "last-modified", "x-ratelimit-remaining", "retry-after", "link"}})
            headers["http_status"] = response.status
            body = response.read(MAX_BYTES + 1)
            if len(body) > MAX_BYTES:
                raise ValueError(f"response exceeded transport byte limit {MAX_BYTES}; evidence incomplete")
        payload = json.loads(body.decode("utf-8"))
        if not isinstance(payload, (dict, list)):
            raise ValueError("expected a JSON object or array")
        return payload, headers
    except HTTPError as exc:
        headers.update({"http_status": exc.code, "retry-after": exc.headers.get("Retry-After"),
                        "x-ratelimit-remaining": exc.headers.get("X-RateLimit-Remaining")})
        return {"error": f"HTTPError: {exc.code} {exc.reason}"}, headers
    except (URLError, HTTPException, TimeoutError, OSError, ValueError, UnicodeDecodeError) as exc:
        return {"error": f"{type(exc).__name__}: {str(exc)[:500]}"}, headers


def _key(name: str, ecosystem: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower() if ecosystem == "pypi" else name


def _repo_name(value: str) -> str | None:
    value = re.sub(r"^git\+", "", value)
    value = re.sub(r"^(?:git|ssh)://", "https://", value)
    value = value.replace("git@github.com:", "https://github.com/")
    if value.startswith("github:"):
        value = "https://github.com/" + value[7:]
    parsed = urlsplit(value)
    if parsed.hostname != "github.com":
        return None
    parts = parsed.path.strip("/").split("/")
    name = "/".join(parts[:2]).removesuffix(".git")
    return name if len(parts) >= 2 and REPO_NAME.fullmatch(name) else None


def _manifest_files(source: Path):
    for root, dirs, files in os.walk(source, followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in EXCLUDED_DIRS and not (Path(root) / d).is_symlink())
        for name in sorted(files):
            path = Path(root) / name
            supported = name in {"pyproject.toml", "package.json", "package-lock.json", "npm-shrinkwrap.json"}
            declared = name in {'setup.py', 'setup.cfg', 'uv.lock', 'poetry.lock', 'Pipfile', 'Pipfile.lock',
                                'yarn.lock', 'pnpm-lock.yaml', 'Cargo.toml', 'Cargo.lock', 'go.mod', 'go.sum'}
            requirements = name.startswith(('requirements', 'constraints')) and name.endswith(('.txt', '.in'))
            if (supported or declared or requirements) and not path.is_symlink():
                yield path


def _add(targets: dict, name: str, ecosystem: str, declaration: dict | None = None) -> None:
    pattern = PYTHON_NAME if ecosystem == "pypi" else NPM_NAME
    if not pattern.fullmatch(name):
        raise ValueError(f"invalid {ecosystem} package name: {name!r}")
    target = targets[ecosystem].setdefault(_key(name, ecosystem), {
        "package": name, "declarations": [], "installed": [], "origins": []})
    if declaration is not None:
        target["declarations"].append(declaration)
        target["origins"].append(f"{declaration['manifest']}:{declaration['group']}")


def _python_requirements(data: dict):
    project = data.get("project", {})
    yield "project.dependencies", project.get("dependencies", [])
    for name, requirements in project.get("optional-dependencies", {}).items():
        yield f"project.optional-dependencies.{name}", requirements
    for name, requirements in data.get("dependency-groups", {}).items():
        yield f"dependency-groups.{name}", requirements
    yield "build-system.requires", data.get("build-system", {}).get("requires", [])
    poetry = data.get("tool", {}).get("poetry", {})
    for group, values in [("dependencies", poetry.get("dependencies", {})),
                          ("dev-dependencies", poetry.get("dev-dependencies", {}))]:
        yield f"tool.poetry.{group}", [{"name": name, "requirement": requirement}
                                      for name, requirement in values.items() if name != "python"]
    for group, values in poetry.get("group", {}).items():
        yield f"tool.poetry.group.{group}.dependencies", [
            {"name": name, "requirement": requirement} for name, requirement in values.get("dependencies", {}).items()]


def _read_python(data: dict, manifest: str, targets: dict, notices: list) -> None:
    for group, requirements in _python_requirements(data):
        if not isinstance(requirements, list):
            raise ValueError(f"{group} must be a list")
        for requirement in requirements:
            if isinstance(requirement, dict) and "include-group" in requirement:
                notices.append({"manifest": manifest, "group": group, "include_group": requirement["include-group"],
                                "note": "All dependency groups are inventoried; group activation is not evaluated."})
                continue
            name = requirement.get("name") if isinstance(requirement, dict) else None
            if isinstance(requirement, str):
                match = re.match(r"\s*([A-Za-z0-9][A-Za-z0-9_.-]*)", requirement)
                name = match.group(1) if match else None
            if not isinstance(name, str):
                raise ValueError(f"invalid requirement in {group}: {requirement!r}")
            _add(targets, name, "pypi", {"manifest": manifest, "group": group, "requirement": requirement})
    if "dependencies" in data.get("project", {}).get("dynamic", []):
        notices.append({"manifest": manifest, "limitation": "Dynamic Python dependencies are not evaluated."})


def _read_node(data: dict, manifest: str, targets: dict) -> None:
    for group in ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies"):
        for name, requirement in data.get(group, {}).items():
            if not isinstance(requirement, str):
                raise ValueError(f"invalid npm requirement for {name}")
            _add(targets, name, "npm", {"manifest": manifest, "group": group, "requirement": requirement})


def _lock_entries(data: dict):
    if isinstance(data.get("packages"), dict):
        for location, item in data["packages"].items():
            if "node_modules/" in location and isinstance(item, dict):
                yield item.get("name") or location.rsplit("node_modules/", 1)[1], item, location
    else:
        def walk(dependencies, prefix=""):
            for name, item in dependencies.items():
                if isinstance(item, dict):
                    location = f"{prefix}node_modules/{name}"
                    yield name, item, location
                    yield from walk(item.get("dependencies", {}), location + "/")
        yield from walk(data.get("dependencies", {}))


def _inventory(source: Path) -> tuple[dict, list, list]:
    targets: dict = {"pypi": {}, "npm": {}}
    manifests, notices, locks = [], [], []
    if not source.is_dir():
        notices.append({"source": str(source), "error": "source directory is missing; declaration coverage unavailable"})
    for path in _manifest_files(source):
        manifest = path.relative_to(source).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
            if path.name.startswith(('requirements', 'constraints')) and path.suffix in ('.txt', '.in'):
                for line in text.splitlines():
                    requirement = line.strip()
                    if not requirement or requirement.startswith('#'):
                        continue
                    if requirement.startswith('-') or requirement.endswith('\\'):
                        notices.append({'manifest': manifest, 'limitation': 'Requirement option/include/continuation is not evaluated',
                                        'declaration': requirement})
                        continue
                    _read_python({'project': {'dependencies': [requirement]}}, manifest, targets, notices)
                manifests.append({'path': manifest, 'status': 'read'})
                continue
            if path.name not in {'pyproject.toml', 'package.json', 'package-lock.json', 'npm-shrinkwrap.json'}:
                manifests.append({'path': manifest, 'status': 'unsupported',
                                  'limitation': 'Discovered declaration/lock format is retained in source_snapshot but not interpreted'})
                continue
            data = tomllib.loads(text) if path.suffix == ".toml" else json.loads(text)
            if path.name == "pyproject.toml":
                _read_python(data, manifest, targets, notices)
            elif path.name == "package.json":
                _read_node(data, manifest, targets)
            else:
                locks.append((manifest, data))
            manifests.append({"path": manifest, "status": "read"})
        except (OSError, ValueError, TypeError, AttributeError) as exc:
            manifests.append({"path": manifest, "status": "error", "error": str(exc)})
    for manifest, data in locks:
        for name, item, location in _lock_entries(data):
            if name in targets["npm"] and item.get("version"):
                targets["npm"][name]["installed"].append({"version": item["version"], "source": manifest,
                    "location": location, "kind": "lockfile_resolution", "verified_runtime_install": False})
    return targets, manifests, notices


def _input_lists(raw_input: dict) -> dict:
    if not isinstance(raw_input, dict):
        raise ValueError("research input must be an object")
    result = {}
    for key, pattern in (("github_repos", REPO_NAME), ("python_packages", PYTHON_NAME), ("npm_packages", NPM_NAME)):
        values = raw_input.get(key, [])
        if not isinstance(values, list) or any(not isinstance(v, str) or not pattern.fullmatch(v) for v in values):
            raise ValueError(f"{key} must be a list of valid names")
        result[key] = values
    return result


def _load_evidence(source: Path) -> tuple[dict, dict, list]:
    if source.name != 'source_snapshot':
        return {}, {}, [{'source': str(source), 'limitation': 'standalone source; no collector window/environment supplied'}]
    root = source.parent
    values, notices = {}, []
    for name in ("index", "environment"):
        path = root / f"{name}.json"
        try:
            values[name] = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(values[name], dict):
                raise ValueError("expected object")
        except (OSError, ValueError) as exc:
            values[name] = {}
            notices.append({"source": str(path), "error": str(exc)})
    return values["index"], values["environment"], notices


def _installed_python(environment: dict, targets: dict) -> None:
    inventory = environment.get("python", {}).get("packages", environment.get("collector_environment", {}).get(
        "distributions", environment.get("python_packages", environment.get("installed_packages", []))))
    if isinstance(inventory, dict):
        inventory = [{"name": name, "version": version} for name, version in inventory.items()]
    for item in inventory if isinstance(inventory, list) else []:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            key = _key(item["name"], "pypi")
            if key in targets["pypi"]:
                targets["pypi"][key]["installed"].append({"version": item.get("version"),
                    "source": "environment.json", "kind": "collector_python_inventory",
                    "verified_service_install": False})


def _package(name: str, ecosystem: str) -> dict:
    encoded = quote(name, safe="")
    endpoint = f"https://pypi.org/pypi/{encoded}/json" if ecosystem == "pypi" else f"https://registry.npmjs.org/{encoded}/latest"
    payload, headers = _get(endpoint)
    result = {"package": name, "ecosystem": ecosystem, "endpoint": endpoint, "retrieved_at": _now(), "headers": headers}
    if not isinstance(payload, dict) or "error" in payload:
        result["error"] = payload.get("error", "unexpected registry response") if isinstance(payload, dict) else "unexpected registry response"
        result["data"] = payload
        return result
    if ecosystem == "pypi":
        info = payload.get("info") or {}
        result["data"] = {"info": {key: info.get(key) for key in ("name", "version", "summary", "home_page", "project_urls")},
                          "latest_files": [{key: item.get(key) for key in ("upload_time_iso_8601", "yanked", "yanked_reason")}
                                           for item in payload.get("urls", [])]}
        result["latest_version"] = info.get("version")
    else:
        result["data"] = {key: payload.get(key) for key in ("name", "version", "description", "repository", "homepage", "bugs")}
        result["latest_version"] = payload.get("version")
    return result


def _repository_links(package: dict):
    data = package.get("data") or {}
    if not isinstance(data, dict) or package.get("error"):
        return
    if package["ecosystem"] == "pypi":
        info = data.get("info", {})
        yield from (info.get("project_urls") or {}).values()
        yield info.get("home_page")
    else:
        repository = data.get("repository")
        yield repository.get("url") if isinstance(repository, dict) else repository
        yield data.get("homepage")
        yield (data.get("bugs") or {}).get("url") if isinstance(data.get("bugs"), dict) else None


def _instant(value: str | None) -> datetime | None:
    if not value:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("evidence window timestamps must include timezone")
    return parsed


def _in_window(value: str | None, window: dict) -> bool:
    date = _instant(value)
    return bool(date and (not window.get("start") or date >= _instant(window["start"]))
                and (not window.get("end_exclusive") or date < _instant(window["end_exclusive"])))


def _pages(endpoint: str, window: dict, timestamp: str, *, issues=False) -> dict:
    result: dict = {"items": [], "requests": [], "errors": [], "coverage": "all_available_pages" if window else "first_page_recent_sample"}
    seen = set()
    while endpoint:
        if endpoint in seen:
            result["errors"].append({"endpoint": endpoint, "error": "repeated pagination URL"})
            break
        seen.add(endpoint)
        payload, headers = _get(endpoint)
        result["requests"].append({"endpoint": endpoint, "headers": headers})
        if not isinstance(payload, list):
            result["errors"].append({"endpoint": endpoint, "error": payload.get("error", "unexpected response") if isinstance(payload, dict) else "unexpected response"})
            break
        for item in payload:
            if not isinstance(item, dict):
                result["errors"].append({"endpoint": endpoint, "error": "non-object activity entry"})
                continue
            if issues and "pull_request" in item:
                continue
            try:
                if window and not _in_window(item.get(timestamp), window):
                    continue
            except (TypeError, ValueError, AttributeError) as exc:
                result["errors"].append({"endpoint": endpoint, "error": f"invalid activity timestamp: {exc}"})
                continue
            fields = ("number", "title", "body", "html_url", "created_at", "updated_at", "closed_at", "state", "comments") if issues else (
                "tag_name", "name", "body", "html_url", "created_at", "published_at", "prerelease", "draft")
            result["items"].append({key: item.get(key) for key in fields})
        match = re.search(r'<([^>]+)>;\s*rel="next"', headers.get("link", ""))
        try:
            endpoint = _url(match.group(1)) if match and window else ""
        except ValueError as exc:
            result["errors"].append({"endpoint": endpoint, "error": str(exc)})
            break
    result["status"] = "partial" if result["errors"] else "retrieved"
    return result


def _repo(repo: str, window: dict) -> dict:
    endpoint = f"https://api.github.com/repos/{repo}"
    metadata, headers = _get(endpoint)
    since = {"since": window["start"]} if window.get("start") else {}
    releases = _pages(endpoint + "/releases?per_page=100", window, "published_at")
    issues = _pages(endpoint + "/issues?" + urlencode({"state": "all", "sort": "updated", "direction": "desc", "per_page": 100, **since}), window, "updated_at", issues=True)
    errors = ([] if isinstance(metadata, dict) and "error" not in metadata else [
        {"endpoint": endpoint, "error": metadata.get("error", "unexpected metadata") if isinstance(metadata, dict) else "unexpected metadata"}])
    errors += releases["errors"] + issues["errors"]
    return {"repo": repo, "retrieved_at": _now(), "metadata": metadata, "headers": headers,
            "releases": releases, "recent_issues": issues, "errors": errors,
            "status": "partial" if errors else "retrieved", "endpoints": [endpoint] + [
                request["endpoint"] for group in (releases, issues) for request in group["requests"]],
            "discussions": {"status": "not_collected", "reason": "Public REST signals do not cover GitHub Discussions; no authenticated GraphQL source is configured."}}


def _source_repos(source: Path, environment: dict) -> list[str]:
    remotes = environment.get("git_remotes", [])
    if isinstance(remotes, dict):
        remotes = list(remotes.values())
    if not isinstance(remotes, list):
        remotes = []
    if (source / ".git").exists():
        try:
            result = subprocess.run(["git", "-C", str(source), "config", "--get-regexp", r"^remote\..*\.url$"],
                                    capture_output=True, text=True, timeout=10, check=False)
            remotes += [line.split(None, 1)[1] for line in result.stdout.splitlines() if " " in line]
        except (OSError, subprocess.TimeoutExpired):
            pass
    return sorted({repo for value in remotes if isinstance(value, str) and (repo := _repo_name(value))})


def research(source: Path, output: Path, raw_input: dict) -> dict:
    extras = _input_lists(raw_input)
    targets, manifests, notices = _inventory(source)
    index, environment, evidence_errors = _load_evidence(source)
    window = {key: index[key] for key in ("start", "end_exclusive") if index.get(key)}
    for value in window.values():
        _instant(value)
    if window.get("start") and window.get("end_exclusive") and _instant(window["start"]) >= _instant(window["end_exclusive"]):
        raise ValueError("evidence window must have start before end_exclusive")
    for ecosystem, key in (("pypi", "python_packages"), ("npm", "npm_packages")):
        for name in extras[key]:
            _add(targets, name, ecosystem)
            targets[ecosystem][_key(name, ecosystem)]["origins"].append(f"input.{key}")
    _installed_python(environment, targets)
    jobs = [(name, ecosystem, inventory) for ecosystem, packages in targets.items() for name, inventory in packages.items()]
    with ThreadPoolExecutor(max_workers=TRANSPORT_WORKERS) as pool:
        packages = list(pool.map(lambda job: {**_package(job[0], job[1]), **job[2]}, jobs))
    repos: dict[str, list[str]] = {}
    for name in extras["github_repos"]:
        repos.setdefault(name, []).append("input.github_repos")
    for name in _source_repos(source, environment):
        repos.setdefault(name, []).append("source.git_remote")
    for package in packages:
        discovered = sorted({repo for value in _repository_links(package) if isinstance(value, str) and (repo := _repo_name(value))})
        package["github_repositories"] = discovered
        package["repository_discovery"] = "found" if discovered else "registry_error" if package.get("error") else "no_github_repository_in_registry_metadata"
        for name in discovered:
            repos.setdefault(name, []).append(f"{package['ecosystem']}:{package['package']}")
    with ThreadPoolExecutor(max_workers=TRANSPORT_WORKERS) as pool:
        github = list(pool.map(lambda item: {**_repo(item[0], window), "origins": sorted(set(item[1]))}, sorted(repos.items())))
    result = {"retrieved_at": _now(), "method": "Public registry latest-version metadata; registry/source-derived GitHub releases and issue signals.",
        "source": {"path": str(source), "frozen_snapshot": source.name == "source_snapshot",
                   "fallback": source.name != "source_snapshot"}, "window": window,
        "github": github, "pypi": [p for p in packages if p["ecosystem"] == "pypi"],
        "npm": [p for p in packages if p["ecosystem"] == "npm"],
        "coverage": {"manifests": manifests, "inventory_notices": notices, "evidence_errors": evidence_errors,
                     "input_lists": "augment discovered declarations and repositories; no target count cap",
                     "declared_package_count": len(packages), "repository_count": len(github),
                     "transport": {"workers": TRANSPORT_WORKERS, "timeout_seconds": TIMEOUT_SECONDS, "response_byte_limit": MAX_BYTES}},
        "limitations": [
            "Public metadata is a signal, not full internet/community coverage or compatibility acceptance; private sources, GitHub Discussions, standalone CHANGELOG files and non-GitHub repository activity are not fetched.",
            "Release bodies provide upstream change notes; releases are filtered by published_at and issues by updated_at in the collector's [start, end_exclusive) window. Pull requests are excluded.",
            "Without a collector window, activity is an explicit first-page recent sample (100 API entries including excluded PRs); with a window all available pages are requested, subject to recorded API/transport errors.",
            "Latest registry metadata and repository metadata describe retrieval time, not a reconstruction at the window cutoff.",
            "Python versions reflect the collector interpreter when supplied, which may differ from the running service; npm lock resolutions do not prove installed runtime versions. Missing evidence is unknown.",
            "Supported Python/npm manifests, requirements files and optional/dev/build groups are inventoried; discovered unsupported manifest formats are listed. Activation markers, dynamic declarations and transitive dependency expansion are not evaluated.",
            "Input lists augment defaults. Every discovered package target is retained even if metadata or repository discovery fails."]}
    output.mkdir(parents=True, exist_ok=True)
    (output / "ecosystem.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    lines = ["# Public ecosystem evidence", "", f"Retrieved: {result['retrieved_at']}", f"Source: {source}; frozen_snapshot={result['source']['frozen_snapshot']}",
             f"Window: {json.dumps(window)}", "", *result["limitations"], ""]
    lines += ["## Inventory and evidence notices", "", json.dumps({
        "manifest_errors": [m for m in manifests if m["status"] == "error"],
        "inventory_notices": notices, "evidence_errors": evidence_errors}, ensure_ascii=False), ""]
    for group in ("github", "pypi", "npm"):
        lines += [f"## {group}", ""]
        for item in result[group]:
            errors = item.get("errors") or ([{"error": item["error"]}] if item.get("error") else [])
            status = "partial/error: " + json.dumps(errors, ensure_ascii=False) if errors else "retrieved"
            lines.append(f"- `{item.get('repo') or item.get('package')}`: {status}; retrieved_at={item['retrieved_at']}; URLs=" + ", ".join(item.get("endpoints") or [item["endpoint"]]))
        lines.append("")
    (output / "sources.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return {"retrieved_at": result["retrieved_at"], **{group: len(result[group]) for group in ("github", "pypi", "npm")},
            "errors": sum(bool(p.get("error") or p.get("errors")) for p in [*packages, *github])}


def main() -> None:
    source = Path("/in/collect/evidence/source_snapshot")
    print(json.dumps(research(source, Path("research"), json.loads(os.environ.get("ANCHOR_INPUT", "{}"))), ensure_ascii=False))


if __name__ == "__main__":
    main()
