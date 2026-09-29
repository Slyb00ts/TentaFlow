#!/usr/bin/env python3
# =============================================================================
# Plik: publish-release-recovery.py
# Opis: Weryfikuje komplet artefaktów beta.1 i publikuje sprawdzony prerelease.
# =============================================================================
import argparse
from datetime import datetime, timezone
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from urllib.error import HTTPError
from urllib.parse import quote, urlsplit
from urllib.request import Request, build_opener, urlopen
import zipfile

_module_spec = importlib.util.spec_from_file_location(
    "native_recovery", Path(__file__).with_name("hydrate-release-native.py"))
native = importlib.util.module_from_spec(_module_spec)
_module_spec.loader.exec_module(native)
require = native.require
api = native.api
TAG = "v0.4.0-beta.1"
VERSION = "0.4.0-beta.1"
SOURCE_SHA = "c7220f148747718ab7b511787de77dc104bffa38"
ANDROID_SHA = "a11124f3891b65aacbee2bd9fc95a29fa79f79e9"
LINUX_SHA = "e90483ab6250aa89f85ecc3ca52b5f5b75a94389"
MAIN_RUN = 36532842108
ANDROID_RUN = 36554217278
ANDROID_JOB = 109359382138
PROVENANCE_FILE = "release-provenance.json"
INSTALLERS = ["install.sh", "uninstall.sh", "install.ps1", "uninstall.ps1"]
ANDROID_PATHS = [
    ".github/workflows/android-memory-diagnostic.yml",
    ".github/workflows/android.yml",
    ".github/workflows/arrow-configure-diagnostic.yml",
    ".github/workflows/cancel-superseded-android.yml",
    ".github/workflows/job-logs-diagnostic.yml",
    ".github/workflows/rerun-release-job.yml",
    "scripts/build-zvec.sh",
]
ANDROID_DIFF_SHA256 = 'c120f86185ace9f760519a610b847e63793615326985f250126ee07c320f8c46'
LINUX_DIFF_SHA256 = 'ab162f79ef1b05a648ad9effc59184b4bbbffe5480b518fc29c32665d0ec4193'
MAIN_JOBS = {'build (macos-full)': 109290168030, 'build (windows) / native-libs (windows-x86_64, vulkan)': 109290168272, 'build (windows) / native-libs (windows-x86_64, cuda)': 109290168332, 'build (windows) / build (windows-x86_64, full-cuda13)': 109290595864, 'build (windows) / build (windows-x86_64, slim)': 109290595873, 'build (windows) / build (windows-x86_64, full-vulkan)': 109290595940, 'build (windows) / install / install (windows-x86_64, full-cuda13)': 109330957292, 'build (windows) / install / install (windows-x86_64, full-vulkan)': 109330957365, 'build (windows) / install / install (windows-x86_64, slim)': 109330957421}
MAIN_ARTIFACTS = {'tentaflow-macos-arm64-full-metal': {'id': 11025156652, 'name': 'tentaflow-macos-arm64-full-metal', 'size_in_bytes': 120432365, 'digest': 'sha256:311ee58541780a43ffb56ebb6a26a526aa354a97ce54a3f045ba4d757b00492b'}, 'tentaflow-windows-x86_64-full-cuda13': {'id': 11022172615, 'name': 'tentaflow-windows-x86_64-full-cuda13', 'size_in_bytes': 806267438, 'digest': 'sha256:2abac687e70681130b74c0b9965e9b89a35cfddd0595e2fc0949c11ca2061a9b'}, 'tentaflow-windows-x86_64-full-vulkan': {'id': 11021302436, 'name': 'tentaflow-windows-x86_64-full-vulkan', 'size_in_bytes': 154700848, 'digest': 'sha256:8570c2f6e4c7714c2e628e914518e82e9cd2288e15a37fe390d12b475240d51b'}, 'tentaflow-windows-x86_64-slim': {'id': 11020373841, 'name': 'tentaflow-windows-x86_64-slim', 'size_in_bytes': 104366695, 'digest': 'sha256:993fa215b9d46db2cf0714b999c7c920ed66472681ab8a70c7a434718370f0ae'}}


def git(*args):
    return native.git(*args)


def source_file(path):
    return git('show', f'{SOURCE_SHA}:{path}')


def check_sources():
    for ref, sha in [(f'refs/tags/{TAG}', SOURCE_SHA), (ANDROID_SHA, ANDROID_SHA),
                     (LINUX_SHA, LINUX_SHA)]:
        subprocess.run(['git', 'fetch', '--no-tags', '--depth=1', 'origin', ref],
                       check=True, stdout=subprocess.DEVNULL)
        require(git('rev-parse', 'FETCH_HEAD^{commit}').decode().strip() == sha,
                'Źródłowa referencja wskazuje inny commit')
    for sha, digest, extra in [
        (ANDROID_SHA, ANDROID_DIFF_SHA256, []),
        (LINUX_SHA, LINUX_DIFF_SHA256, ['.github/workflows/linux-release-recovery.yml',
                                      'scripts/ci-local/hydrate-release-native.py']),
    ]:
        names = git('diff', '--name-only', SOURCE_SHA, sha).decode().splitlines()
        require(names == sorted(ANDROID_PATHS + extra), 'Niezatwierdzona zmiana źródeł aplikacji')
        patch = git('diff', '--no-ext-diff', '--no-textconv', '--binary', '--no-color',
                    '--full-index', '--no-renames', '--diff-algorithm=myers',
                    '--no-indent-heuristic', '--src-prefix=a/', '--dst-prefix=b/', SOURCE_SHA, sha)
        require(hashlib.sha256(patch).hexdigest() == digest, 'Nieprawidłowy diff receptury CI')
    manifest = tomllib.loads(source_file('Cargo.toml').decode())
    require(manifest['workspace']['package']['version'] == VERSION, 'Nieprawidłowa wersja workspace')
    gradle = source_file('tentaflow-mobile/android/app/build.gradle.kts').decode()
    require(re.search(r'\bversionCode\s*=\s*5\b', gradle)
            and f'versionName = "{VERSION}"' in gradle, 'Nieprawidłowa wersja źródeł Androida')


def policies(linux_run):
    linux_jobs = {}
    for variant in native.SPECS:
        linux_jobs[f'hydrate native ({variant})'] = None
    for platform in ['linux-x86_64', 'linux-aarch64']:
        for asset in ['slim', 'full-vulkan', 'full-cuda12', 'full-cuda13']:
            linux_jobs[f'build ({platform}, {asset})'] = None
        for asset in ['slim', 'full-vulkan']:
            linux_jobs[f'verify ({platform}, {asset})'] = None
    return [
        {'id': MAIN_RUN, 'sha': SOURCE_SHA, 'path': '.github/workflows/release.yml',
         'require_success': False, 'jobs': MAIN_JOBS},
        {'id': ANDROID_RUN, 'sha': ANDROID_SHA,
         'path': '.github/workflows/android-memory-diagnostic.yml', 'require_success': True,
         'jobs': {'android / build (arm64-v8a, unsigned)': ANDROID_JOB}},
        {'id': linux_run, 'sha': LINUX_SHA, 'path': '.github/workflows/linux-release-recovery.yml',
         'require_success': True, 'jobs': linux_jobs},
    ]


def collect(path, key=None):
    result = []
    for page in range(1, 101):
        response = api(f'{path}{"&" if "?" in path else "?"}per_page=100&page={page}')
        items = response[key] if key else response
        result.extend(items)
        if len(items) < 100:
            return result
    raise ValueError('Przekroczony limit stronicowania GitHub API')


def check_run(policy, run):
    require(run['id'] == policy['id'] and run['head_sha'] == policy['sha']
            and run['repository']['id'] == native.REPOSITORY_ID
            and run['head_repository']['id'] == native.REPOSITORY_ID
            and run['path'] == policy['path'], 'Nieprawidłowy źródłowy run')
    if policy['require_success']:
        require(run['status'] == 'completed' and run['conclusion'] == 'success',
                'Wymagany run nie zakończył się sukcesem')


def check_job(policy, name, job):
    require(job['name'] == name and job['run_id'] == policy['id']
            and job['head_sha'] == policy['sha'] and job['status'] == 'completed'
            and job['conclusion'] == 'success', f'Wymagany job nie przeszedł: {name}')
    if policy['jobs'][name] is not None:
        require(job['id'] == policy['jobs'][name], 'Identyfikator źródłowego joba zmienił się')


def evidence(linux_run):
    result = {}
    for policy in policies(linux_run):
        run = api(f'actions/runs/{policy["id"]}')
        check_run(policy, run)
        jobs = collect(f'actions/runs/{policy["id"]}/jobs?filter=latest', 'jobs')
        selected = {}
        for name in policy['jobs']:
            matching = [job for job in jobs if job['name'] == name]
            require(len(matching) == 1, f'Brak jednoznacznego joba: {name}')
            check_job(policy, name, matching[0])
            selected[name] = matching[0]
        artifacts = collect(f'actions/runs/{policy["id"]}/artifacts', 'artifacts')
        result[policy['id']] = {'policy': policy, 'run': run, 'jobs': selected, 'artifacts': artifacts}
    return result


def packages(linux_run):
    result = []
    for asset in ['slim', 'full-vulkan', 'full-cuda13']:
        result.append({'run_id': MAIN_RUN, 'artifact': f'tentaflow-windows-x86_64-{asset}',
                       'job': f'build (windows) / build (windows-x86_64, {asset})',
                       'file': f'tentaflow-{TAG}-x86_64-pc-windows-msvc-{asset}.zip'})
    result.append({'run_id': MAIN_RUN, 'artifact': 'tentaflow-macos-arm64-full-metal',
                   'job': 'build (macos-full)',
                   'file': f'tentaflow-{TAG}-aarch64-apple-darwin-full-metal.tar.gz'})
    result.append({'run_id': ANDROID_RUN, 'artifact': 'tentaflow-android-arm64-v8a-debug',
                   'job': 'android / build (arm64-v8a, unsigned)',
                   'file': f'tentaflow-{TAG}-android-arm64-v8a-debug.apk'})
    for platform, target in [('linux-x86_64', 'x86_64-unknown-linux-gnu'),
                             ('linux-aarch64', 'aarch64-unknown-linux-gnu')]:
        for asset in ['slim', 'full-vulkan', 'full-cuda12', 'full-cuda13']:
            result.append({'run_id': linux_run, 'artifact': f'tentaflow-{platform}-{asset}',
                           'job': f'build ({platform}, {asset})',
                           'file': f'tentaflow-{TAG}-{target}-{asset}.tar.gz'})
    return result


def check_artifact(artifact, run_id, sha, job, expected_name):
    origin = artifact['workflow_run']
    require(artifact['name'] == expected_name and isinstance(artifact['id'], int)
            and artifact['id'] > 0 and 0 < artifact['size_in_bytes'] <= 5 * 1024**3
            and re.fullmatch(r'sha256:[0-9a-f]{64}', artifact['digest'] or ''),
            'Nieprawidłowe metadane artefaktu')
    require(origin['id'] == run_id and origin['head_sha'] == sha
            and origin['repository_id'] == native.REPOSITORY_ID
            and origin['head_repository_id'] == native.REPOSITORY_ID,
            'Nieprawidłowe pochodzenie artefaktu')
    require(artifact['expired'] is False and
            datetime.fromisoformat(artifact['expires_at'].replace('Z', '+00:00')) >
            datetime.now(timezone.utc), 'Artefakt wygasł')
    require(job['started_at'] <= artifact['created_at'] <= job['completed_at'],
            'Artefakt powstał poza źródłowym jobem')
    if expected_name in MAIN_ARTIFACTS:
        require(all(artifact[key] == value for key, value in MAIN_ARTIFACTS[expected_name].items()),
                'Istniejący artefakt Windows/macOS zmienił się')


def artifact_pin(item, proof):
    matches = [a for a in proof['artifacts'] if a['name'] == item['artifact']]
    require(len(matches) == 1, 'Brak jednoznacznego artefaktu')
    artifact = matches[0]
    job = proof['jobs'][item['job']]
    check_artifact(artifact, item['run_id'], proof['policy']['sha'], job, item['artifact'])
    return {key: artifact[key] for key in ['id', 'name', 'size_in_bytes', 'digest',
                                          'created_at', 'expires_at', 'workflow_run']}


def extract_files(archive_path, destination, expected):
    # Archiwum Actions zawiera wyłącznie konkretne pliki wydania, bez katalogów.
    with zipfile.ZipFile(archive_path) as archive:
        entries = archive.infolist()
        require(len(entries) == len(expected) and {e.filename for e in entries} == set(expected),
                'Nieoczekiwana zawartość artefaktu wydania')
        for entry in entries:
            require('/' not in entry.filename and '\\' not in entry.filename
                    and stat.S_IFMT(entry.external_attr >> 16) in (0, stat.S_IFREG)
                    and not entry.is_dir() and not (entry.flag_bits & 1)
                    and 0 < entry.file_size <= 5 * 1024**3, 'Nieprawidłowy plik w artefakcie')
        require(sum(e.file_size for e in entries) <= 5 * 1024**3, 'Artefakt jest zbyt duży')
        destination.mkdir(parents=True, exist_ok=True)
        for entry in entries:
            with archive.open(entry) as source, (destination / entry.filename).open('xb') as output:
                shutil.copyfileobj(source, output)


def sha256(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def check_checksum(path):
    checksum = path.with_name(path.name + '.sha256').read_text()
    match = re.fullmatch(r'([0-9a-fA-F]{64}) [ *]([^\r\n]+)\r?\n?', checksum)
    require(match and match[2] == path.name and match[1].lower() == sha256(path),
            'Nieprawidłowa suma kontrolna pakietu')


def check_apk(path):
    sdk = Path(os.environ['ANDROID_HOME'])
    aapt = sorted(sdk.glob('build-tools/*/aapt2'))
    require(aapt, 'Brak aapt2 do sprawdzenia wersji APK')
    result = subprocess.check_output([str(aapt[-1]), 'dump', 'badging', str(path)], text=True)
    package = next((line for line in result.splitlines() if line.startswith('package: ')), '')
    require("name='ai.tentaflow.mobile'" in package and "versionCode='5'" in package
            and f"versionName='{VERSION}'" in package, 'Nieprawidłowa wersja lub ID aplikacji APK')
    signer = aapt[-1].with_name('apksigner')
    require(signer.is_file(), 'Brak apksigner do sprawdzenia podpisu APK')
    subprocess.run([str(signer), 'verify', str(path)], check=True)
    with tempfile.TemporaryDirectory() as scratch:
        checker = Path(scratch) / 'check-android-apk.py'
        checker.write_bytes(source_file('scripts/ci-local/check-android-apk.py'))
        subprocess.run([sys.executable, str(checker), str(path), '--abi', 'arm64-v8a'], check=True)


def notes():
    workflow = source_file('.github/workflows/release.yml').decode()
    body = workflow.split('          body: |\n', 1)[1].split('          generate_release_notes:', 1)[0]
    body = '\n'.join(line[12:] if line.startswith('            ') else line
                     for line in body.rstrip().splitlines())
    body = body.replace('${{ github.ref_name }}', TAG)
    return body + (f"\n\nBuild provenance: [release-provenance.json]"
                   f"(https://github.com/{native.REPOSITORY}/releases/download/"
                   f"{TAG}/{PROVENANCE_FILE}).\n")


def job_records(proof):
    return [{'id': job['id'], 'name': job['name'], 'run_attempt': job['run_attempt'],
             'head_sha': job['head_sha']} for job in proof['jobs'].values()]


def find_existing_release():
    require(not any(release['tag_name'] == TAG for release in collect('releases')),
            'Wydanie dla tagu już istnieje; nie nadpisujemy żadnych załączników')


def expected_files(linux_run):
    package_names = [item['file'] for item in packages(linux_run)]
    return set(package_names + [name + '.sha256' for name in package_names]
               + INSTALLERS + [PROVENANCE_FILE])


def prepare(linux_run):
    check_sources()
    find_existing_release()
    proofs = evidence(linux_run)
    work = Path('release-recovery')
    require(not work.exists(), 'Katalog przygotowanego wydania już istnieje')
    dist = work / 'dist'
    dist.mkdir(parents=True)
    records = []
    for item in packages(linux_run):
        pin = artifact_pin(item, proofs[item['run_id']])
        with tempfile.TemporaryDirectory() as scratch:
            archive = Path(scratch) / 'artifact.zip'
            native.download(pin, archive)
            extract_files(archive, dist, [item['file'], item['file'] + '.sha256'])
        check_checksum(dist / item['file'])
        records.append(dict(item, artifact_pin=pin, sha256=sha256(dist / item['file']),
                            size=(dist / item['file']).stat().st_size))
    native_records = []
    for variant, spec in native.SPECS.items():
        item = {'run_id': linux_run, 'artifact': f'native-provenance-{variant}',
                'job': f'hydrate native ({variant})'}
        pin = artifact_pin(item, proofs[linux_run])
        with tempfile.TemporaryDirectory() as scratch:
            scratch = Path(scratch)
            archive = scratch / 'artifact.zip'
            native.download(pin, archive)
            extract_files(archive, scratch / 'extracted', ['provenance.json'])
            record = json.loads((scratch / 'extracted' / 'provenance.json').read_text())
        require(all(record[key] == value for key, value in spec.items())
                and record['application_source_sha'] == SOURCE_SHA
                and record['native_recipe_diff_sha256'] == native.RECIPE_DIFF_SHA256
                and record['recovery_sha'] == LINUX_SHA
                and str(record['recovery_run_id']) == str(linux_run),
                'Nieprawidłowy manifest pochodzenia bibliotek natywnych')
        native_records.append(dict(item, artifact_pin=pin, provenance=record))
    apk = next(dist / item['file'] for item in records if item['file'].endswith('.apk'))
    check_apk(apk)
    for filename in INSTALLERS:
        (dist / filename).write_bytes(source_file('scripts/install/' + filename))
    source_runs = [{'id': run_id, 'head_sha': proof['policy']['sha'],
                    'workflow': proof['policy']['path'], 'jobs': job_records(proof)}
                   for run_id, proof in proofs.items()]
    provenance = {
        'tag': TAG, 'application_source_sha': SOURCE_SHA,
        'android_recipe_sha': ANDROID_SHA, 'android_recipe_diff_sha256': ANDROID_DIFF_SHA256,
        'android_build': {'cargo_jobs': 1, 'release_lto': 'off', 'release_codegen_units': 16,
                          'additional_swap_gib': 16, 'version_name': VERSION, 'version_code': 5,
                          'signing': 'debug'},
        'linux_recipe_sha': LINUX_SHA, 'linux_recipe_diff_sha256': LINUX_DIFF_SHA256,
        'source_runs': source_runs, 'packages': records, 'native_libraries': native_records,
        'installers': [{'name': name, 'sha256': sha256(dist / name)} for name in INSTALLERS],
        'publisher_sha': os.environ['GITHUB_SHA'], 'publisher_run_id': os.environ['GITHUB_RUN_ID'],
        'historical_failures': 'Main run Android and ARM CUDA12 failures are replaced by successful recovery jobs.',
    }
    (dist / PROVENANCE_FILE).write_text(json.dumps(provenance, indent=2) + '\n')
    require({p.name for p in dist.iterdir()} == expected_files(linux_run), 'Niekompletny zestaw wydania')
    assets = [{'name': path.name, 'size': path.stat().st_size, 'sha256': sha256(path)}
              for path in sorted(dist.iterdir())]
    manifest = {'tag': TAG, 'source_sha': SOURCE_SHA, 'linux_run_id': linux_run,
                'publisher_sha': os.environ['GITHUB_SHA'], 'publisher_run_id': os.environ['GITHUB_RUN_ID'],
                'source_runs': source_runs, 'packages': records, 'native_libraries': native_records,
                'assets': assets, 'body': notes()}
    prepared = work / 'prepared.json'
    prepared.write_text(json.dumps(manifest, indent=2) + '\n')
    digest = sha256(prepared)
    if 'GITHUB_OUTPUT' in os.environ:
        with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
            output.write(f'manifest_sha256={digest}\n')
    print(f'Gotowe: 13 pakietów, 13 sum, 4 instalatory i manifest; SHA256 manifestu: {digest}')



def report_api_error(error):
    raw = error.read(16385)
    try:
        payload = json.loads(raw) if len(raw) <= 16384 else {}
    except (ValueError, UnicodeDecodeError):
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    details = {key: payload[key] for key in ['message', 'errors', 'documentation_url'] if key in payload}
    headers = error.headers or {}
    for key in ['X-Accepted-GitHub-Permissions', 'Retry-After',
                'X-RateLimit-Remaining', 'X-RateLimit-Reset']:
        if headers.get(key) is not None:
            details[key] = headers[key]
    text = json.dumps(details, ensure_ascii=False)
    token = os.environ.get('GITHUB_TOKEN')
    if token:
        text = text.replace(token, '[REDACTED]')
    text = re.sub(r'\b(?:gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)\b', '[REDACTED]', text)
    text = re.sub(r'(?i)Bearer\s+[^\s\\"\']+', 'Bearer [REDACTED]', text)
    text = re.sub(r'(?i)((?:token|secret|password|api[_-]?key|authorization)["\']?\s*[:=]\s*["\']?)[^\s"\',}\\]+', r'\1[REDACTED]', text)
    # Adresy z parametrami mogą zawierać podpis uprawniający do pobrania pliku.
    text = re.sub(r'https?://[^\s"\'\\]*\?[^\s"\'\\]*', '[REDACTED_URL]', text)
    text = text[:6000]
    for offset in range(0, len(text), 2000):
        escaped = text[offset:offset + 2000].replace('%', '%25').replace('\r', '%0D').replace('\n', '%0A')
        print(f'::notice title=GitHub API HTTP {error.code}::{escaped}')
    return text


def mutation(method, path, body):
    request = Request(f'https://api.github.com/repos/{native.REPOSITORY}/{path}',
                      data=json.dumps(body).encode(), method=method, headers={
                          'Authorization': f'Bearer {os.environ["GITHUB_TOKEN"]}',
                          'Accept': 'application/vnd.github+json',
                          'Content-Type': 'application/json',
                          'X-GitHub-Api-Version': '2022-11-28',
                          'User-Agent': 'tentaflow-release-recovery'})
    try:
        with build_opener(native.NoRedirect).open(request, timeout=120) as response:
            require(response.status == (201 if method == 'POST' else 200),
                    'Nieoczekiwany status zapisu GitHub API')
            return json.load(response)
    except HTTPError as error:
        details = report_api_error(error)
        raise RuntimeError(f'GitHub zapis odrzucony: HTTP {error.code}; {details}') from None


def upload(release_id, path):
    connection = http.client.HTTPSConnection('uploads.github.com', timeout=300)
    try:
        endpoint = f'/repos/{native.REPOSITORY}/releases/{release_id}/assets?name={quote(path.name)}'
        connection.putrequest('POST', endpoint)
        connection.putheader('Authorization', f'Bearer {os.environ["GITHUB_TOKEN"]}')
        connection.putheader('Content-Type', 'application/octet-stream')
        connection.putheader('Content-Length', str(path.stat().st_size))
        connection.putheader('Accept', 'application/vnd.github+json')
        connection.putheader('User-Agent', 'tentaflow-release-recovery')
        connection.endheaders()
        with path.open('rb') as source:
            while block := source.read(1024 * 1024):
                connection.send(block)
        response = connection.getresponse()
        require(response.status == 201, f'Wysyłanie załącznika odrzucone: HTTP {response.status}')
        return json.load(response)
    finally:
        connection.close()


def asset_digest(asset_id, expected_size):
    request = Request(f'https://api.github.com/repos/{native.REPOSITORY}/releases/assets/{asset_id}',
                      headers={'Authorization': f'Bearer {os.environ["GITHUB_TOKEN"]}',
                               'Accept': 'application/octet-stream',
                               'X-GitHub-Api-Version': '2022-11-28',
                               'User-Agent': 'tentaflow-release-recovery'})
    try:
        response = build_opener(native.NoRedirect).open(request, timeout=120)
    except HTTPError as error:
        require(error.code == 302, f'Pobranie załącznika odrzucone: HTTP {error.code}')
        location = error.headers.get('Location', '')
        url = urlsplit(location)
        require(url.scheme == 'https' and url.hostname and not url.username
                and not url.password and url.port in (None, 443), 'Nieprawidłowy URL załącznika')
        response = urlopen(location, timeout=120)
    digest = hashlib.sha256()
    size = 0
    with response:
        while block := response.read(1024 * 1024):
            size += len(block)
            require(size <= expected_size, 'Zdalny załącznik przekracza oczekiwany rozmiar')
            digest.update(block)
    require(size == expected_size, 'Zdalny załącznik jest niekompletny')
    return digest.hexdigest()


def verify_remote(release_id, assets, uploaded_ids, verify_bytes=True):
    remote = collect(f'releases/{release_id}/assets')
    require(len(remote) == len(assets) and {a['name'] for a in remote} == {a['name'] for a in assets},
            'Niekompletny lub nadmiarowy zestaw zdalnych załączników')
    by_name = {a['name']: a for a in remote}
    for expected in assets:
        actual = by_name[expected['name']]
        require(actual['id'] == uploaded_ids[expected['name']]
                and actual['size'] == expected['size'] and actual['state'] == 'uploaded',
                'Zdalny załącznik ma nieprawidłowe metadane')
        if verify_bytes:
            require(asset_digest(actual['id'], expected['size']) == expected['sha256'],
                    'Zdalny załącznik ma inne bajty niż przygotowane wydanie')


def revalidate(manifest, dist):
    require(manifest['tag'] == TAG and manifest['source_sha'] == SOURCE_SHA
            and manifest['publisher_sha'] == os.environ['GITHUB_SHA']
            and str(manifest['publisher_run_id']) == os.environ['GITHUB_RUN_ID'],
            'Manifest nie pochodzi z tego przebiegu publikacji')
    linux_run = manifest['linux_run_id']
    proofs = evidence(linux_run)
    for saved in manifest['source_runs']:
        require(saved['head_sha'] == proofs[saved['id']]['policy']['sha']
                and saved['jobs'] == job_records(proofs[saved['id']]), 'Źródłowe joby zmieniły się')
    for item in manifest['packages'] + manifest['native_libraries']:
        require(artifact_pin(item, proofs[item['run_id']]) == item['artifact_pin'],
                'Źródłowy artefakt zmienił się po przygotowaniu')
    names = expected_files(linux_run)
    require(len(manifest['assets']) == len(names)
            and {a['name'] for a in manifest['assets']} == names
            and {p.name for p in dist.iterdir()} == names, 'Zmieniony zestaw lokalnych załączników')
    for expected in manifest['assets']:
        path = dist / expected['name']
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == expected['size']
                and sha256(path) == expected['sha256'], 'Lokalny załącznik zmienił się')
    for name in INSTALLERS:
        require((dist / name).read_bytes() == source_file('scripts/install/' + name),
                'Instalator nie pochodzi z tagu wydania')
    for item in manifest['packages']:
        check_checksum(dist / item['file'])
    require(manifest['body'] == notes(), 'Opis wydania zmienił się')


def publish(expected_manifest_sha):
    require(re.fullmatch(r'[0-9a-f]{64}', expected_manifest_sha), 'Nieprawidłowy digest manifestu')
    prepared = Path('release-recovery/prepared.json')
    require(sha256(prepared) == expected_manifest_sha, 'Manifest zmienił się po przygotowaniu')
    manifest = json.loads(prepared.read_text())
    check_sources()
    dist = prepared.parent / 'dist'
    revalidate(manifest, dist)
    find_existing_release()
    release = mutation('POST', 'releases', {'tag_name': TAG,
                       'name': 'TentaFlow 0.4.0 beta.1', 'body': manifest['body'],
                       'draft': True, 'prerelease': True, 'make_latest': 'false'})
    release_id = release['id']
    require(release['draft'] is True and release['prerelease'] is True and release['tag_name'] == TAG,
            'GitHub nie utworzył wymaganego draftu')
    print(f'Utworzono draft wydania {release_id}; załączniki zostaną zweryfikowane przed publikacją')
    (prepared.parent / 'draft-release-id.txt').write_text(str(release_id) + '\n')
    uploaded = {}
    for asset in manifest['assets']:
        response = upload(release_id, dist / asset['name'])
        require(response['name'] == asset['name'] and response['size'] == asset['size']
                and response['state'] == 'uploaded', 'GitHub zapisał nieprawidłowy załącznik')
        uploaded[asset['name']] = response['id']
    verify_remote(release_id, manifest['assets'], uploaded)
    current = api(f'releases/{release_id}')
    require(current['draft'] is True and current['prerelease'] is True and current['tag_name'] == TAG
            and current['body'] == manifest['body'], 'Draft zmienił się podczas publikacji')
    # Pochodzenie i tag są ponownie sprawdzane po wysłaniu wszystkich załączników.
    revalidate(manifest, dist)
    check_sources()
    published = mutation('PATCH', f'releases/{release_id}',
                         {'draft': False, 'prerelease': True, 'make_latest': 'false'})
    require(published['draft'] is False and published['prerelease'] is True
            and published['tag_name'] == TAG, 'GitHub nie potwierdził publikacji prerelease')
    final = api(f'releases/{release_id}')
    require(final['draft'] is False and final['prerelease'] is True
            and final['tag_name'] == TAG and final['body'] == manifest['body'],
            'Końcowy stan wydania jest nieprawidłowy')
    verify_remote(release_id, manifest['assets'], uploaded, verify_bytes=False)
    print(f'Opublikowano i zweryfikowano: https://github.com/{native.REPOSITORY}/releases/tag/{TAG}')


def main():
    parser = argparse.ArgumentParser(description='Kontrolowana publikacja odzyskanego wydania beta.1')
    commands = parser.add_subparsers(dest='command', required=True)
    stage = commands.add_parser('prepare')
    stage.add_argument('--linux-run-id', required=True, type=int)
    ship = commands.add_parser('publish')
    ship.add_argument('--manifest-sha256', required=True)
    args = parser.parse_args()
    require(os.environ.get('GITHUB_REPOSITORY') == native.REPOSITORY, 'Nieprawidłowe repozytorium')
    if args.command == 'prepare':
        require(args.linux_run_id > 0 and args.linux_run_id not in (MAIN_RUN, ANDROID_RUN),
                'Nieprawidłowy identyfikator przebiegu Linux')
        prepare(args.linux_run_id)
    else:
        publish(args.manifest_sha256)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(f'Publikacja nie powiodła się: {type(error).__name__}', file=sys.stderr)
        if isinstance(error, (ValueError, RuntimeError)):
            print(str(error), file=sys.stderr)
        sys.exit(1)
