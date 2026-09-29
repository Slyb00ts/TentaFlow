#!/usr/bin/env python3
# =============================================================================
# Plik: hydrate-release-native.py
# Opis: Odzyskuje zweryfikowane biblioteki Linux dla wydania v0.4.0-beta.1.
# =============================================================================
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from datetime import datetime, timezone
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen
import zipfile

REPOSITORY = "Slyb00ts/TentaFlow"
REPOSITORY_ID = 1190689460
OLD_SHA = "2ea74dad19f292d6284746c3dc41f74a0b69f0d0"
SOURCE_SHA = "c7220f148747718ab7b511787de77dc104bffa38"
RECIPE_DIFF_SHA256 = "ce1cead6766b91e8ff6f0afd1ad7a6ca380164ba1d43b27f00541a1ffdc4e04f"
SPECS = {'linux-x86_64-vulkan': {'id': 11018437896,
                         'name': 'native-libs-linux-x86_64-vulkan',
                         'size_in_bytes': 113047356,
                         'digest': 'sha256:4437367f296c9058a6e4431a91d4eb37802bb92cf7d4b2d31c7d1a141488642d',
                         'platform': 'linux-x86_64',
                         'backend': 'vulkan',
                         'run_id': 36532842108,
                         'source_sha': 'c7220f148747718ab7b511787de77dc104bffa38',
                         'job_id': 109290168134,
                         'job_name': 'native-libs (linux-x86_64, vulkan)'},
 'linux-x86_64-cuda12': {'id': 11025985787,
                         'name': 'native-libs-linux-x86_64-cuda12',
                         'size_in_bytes': 506827928,
                         'digest': 'sha256:cfe86a55ebbeff52a80454220d7880fc65f7f6844b0445be0f01b8107fca3949',
                         'platform': 'linux-x86_64',
                         'backend': 'cuda12',
                         'run_id': 36532842108,
                         'source_sha': 'c7220f148747718ab7b511787de77dc104bffa38',
                         'job_id': 109290168214,
                         'job_name': 'native-libs (linux-x86_64, cuda12)'},
 'linux-x86_64-cuda13': {'id': 10937721177,
                         'name': 'native-libs-linux-x86_64-cuda13',
                         'size_in_bytes': 622804795,
                         'digest': 'sha256:e3c21414e9e3f4e16e271ed5721b87d7ca1b783e7de5a2d6b358e4b96c793ed1',
                         'platform': 'linux-x86_64',
                         'backend': 'cuda13',
                         'run_id': 36322010449,
                         'source_sha': '2ea74dad19f292d6284746c3dc41f74a0b69f0d0',
                         'job_id': 108627534677,
                         'job_name': 'native-libs (linux-x86_64, cuda13)'},
 'linux-aarch64-vulkan': {'id': 11018990053,
                          'name': 'native-libs-linux-aarch64-vulkan',
                          'size_in_bytes': 109485819,
                          'digest': 'sha256:43728b91cc08f6eb1017ec3967ac43b0f6f5e7c23ee1e587be9e256f955944fb',
                          'platform': 'linux-aarch64',
                          'backend': 'vulkan',
                          'run_id': 36532842108,
                          'source_sha': 'c7220f148747718ab7b511787de77dc104bffa38',
                          'job_id': 109290168224,
                          'job_name': 'native-libs (linux-aarch64, vulkan)'},
 'linux-aarch64-cuda12': {'id': 10934424571,
                          'name': 'native-libs-linux-aarch64-cuda12',
                          'size_in_bytes': 499341797,
                          'digest': 'sha256:cafa58391fb58b7f39a52f3cd912f564b8d6f82f6a1b24a47e5fa285b19c5edb',
                          'platform': 'linux-aarch64',
                          'backend': 'cuda12',
                          'run_id': 36322010449,
                          'source_sha': '2ea74dad19f292d6284746c3dc41f74a0b69f0d0',
                          'job_id': 108627534690,
                          'job_name': 'native-libs (linux-aarch64, cuda12)'},
 'linux-aarch64-cuda13': {'id': 11021909087,
                          'name': 'native-libs-linux-aarch64-cuda13',
                          'size_in_bytes': 524662045,
                          'digest': 'sha256:c85e24f162c845436b2403645d52d611504d3d23074c538da2cc885673ac725b',
                          'platform': 'linux-aarch64',
                          'backend': 'cuda13',
                          'run_id': 36532842108,
                          'source_sha': 'c7220f148747718ab7b511787de77dc104bffa38',
                          'job_id': 109290168317,
                          'job_name': 'native-libs (linux-aarch64, cuda13)'}}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def git(*args):
    return subprocess.check_output(['git', *args])


def check_recipes():
    for tag, sha in [('v0.4.0-beta', OLD_SHA), ('v0.4.0-beta.1', SOURCE_SHA)]:
        subprocess.run(['git', 'fetch', '--no-tags', '--depth=1', 'origin',
                        f'refs/tags/{tag}'], check=True, stdout=subprocess.DEVNULL)
        require(git('rev-parse', 'FETCH_HEAD^{commit}').decode().strip() == sha,
                f'Tag {tag} wskazuje inny commit')
    paths = ['scripts', '.github/workflows/release.yml', 'vendor',
             'tentaflow-zvec-sys', '.gitmodules']
    changed = git('diff', '--name-only', OLD_SHA, SOURCE_SHA, '--', *paths)
    require(changed.decode().splitlines() == [
        'scripts/build-zvec.sh', 'scripts/native-libs/test-ios-host-sdk.sh'],
        'Niezatwierdzona zmiana wejść bibliotek natywnych')
    # Digest obejmuje dokładnie dodane wywołania macOS/iOS oraz test Darwin.
    patch = git('-c', 'core.quotePath=true', 'diff', '--no-ext-diff', '--no-textconv',
                '--binary', '--no-color', '--full-index', '--no-renames',
                '--diff-algorithm=myers', '--no-indent-heuristic',
                '--src-prefix=a/', '--dst-prefix=b/',
                OLD_SHA, SOURCE_SHA, '--', *paths)
    require(hashlib.sha256(patch).hexdigest() == RECIPE_DIFF_SHA256,
            'Diff receptur nie odpowiada zatwierdzonym zmianom Darwin')


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


def api(path, redirect=False):
    request = Request(f'https://api.github.com/repos/{REPOSITORY}/{path}', headers={
        'Authorization': f'Bearer {os.environ["GITHUB_TOKEN"]}',
        'Accept': 'application/vnd.github+json',
        'X-GitHub-Api-Version': '2022-11-28',
        'User-Agent': 'tentaflow-release-recovery',
    })
    try:
        with build_opener(NoRedirect).open(request, timeout=60) as response:
            require(not redirect, 'Brak przekierowania pobrania artefaktu')
            return json.load(response)
    except HTTPError as error:
        if redirect and error.code == 302:
            location = error.headers.get('Location', '')
            target = urlsplit(location)
            require(target.scheme == 'https' and target.hostname and not target.username
                    and not target.password and target.port in (None, 443),
                    'Nieprawidłowy adres pobrania artefaktu')
            return location
        raise RuntimeError(f'GitHub API zwróciło HTTP {error.code}') from None


def check_metadata(spec, run, job, artifact):
    require(run['id'] == spec['run_id'] and run['head_sha'] == spec['source_sha']
            and run['repository']['id'] == REPOSITORY_ID
            and run['head_repository']['id'] == REPOSITORY_ID
            and run['path'] == '.github/workflows/release.yml',
            'Niezgodne pochodzenie źródłowego workflow')
    require(job['id'] == spec['job_id'] and job['run_id'] == spec['run_id']
            and job['head_sha'] == spec['source_sha'] and job['name'] == spec['job_name']
            and job['status'] == 'completed' and job['conclusion'] == 'success',
            'Źródłowy job nie zakończył się sukcesem dla wskazanego commita')
    for key in ['id', 'name', 'size_in_bytes', 'digest']:
        require(artifact[key] == spec[key], f'Niezgodne metadane artefaktu: {key}')
    origin = artifact['workflow_run']
    require(origin['id'] == spec['run_id'] and origin['head_sha'] == spec['source_sha']
            and origin['repository_id'] == REPOSITORY_ID
            and origin['head_repository_id'] == REPOSITORY_ID,
            'Artefakt pochodzi z innego repozytorium lub commita')
    require(artifact['expired'] is False and
            datetime.fromisoformat(artifact['expires_at'].replace('Z', '+00:00')) >
            datetime.now(timezone.utc), 'Artefakt wygasł')
    require(job['started_at'] <= artifact['created_at'] <= job['completed_at'],
            'Artefakt nie został utworzony podczas wskazanego joba')


def download(spec, target):
    location = api(f'actions/artifacts/{spec["id"]}/zip', redirect=True)
    digest = hashlib.sha256()
    size = 0
    # Podpisany URL otrzymuje żądanie bez nagłówka Authorization GitHuba.
    with urlopen(location, timeout=120) as response, target.open('xb') as output:
        while block := response.read(1024 * 1024):
            size += len(block)
            require(size <= spec['size_in_bytes'], 'Artefakt przekracza oczekiwany rozmiar')
            digest.update(block)
            output.write(block)
    require(size == spec['size_in_bytes'], 'Niekompletny artefakt')
    require('sha256:' + digest.hexdigest() == spec['digest'], 'Digest ZIP nie zgadza się')


def extract(source, destination):
    allowed_roots = {'include', 'lib-static', 'lib-dynamic', 'bin', 'manifest.toml'}
    seen = set()
    expanded = 0
    with zipfile.ZipFile(source) as archive:
        entries = archive.infolist()
        require(len(entries) <= 100000, 'Zbyt wiele plików w artefakcie')
        for entry in entries:
            name = entry.filename.rstrip('/')
            parts = name.split('/')
            mode = stat.S_IFMT(entry.external_attr >> 16)
            require(name and '\\' not in name and '\x00' not in name
                    and not PurePosixPath(name).is_absolute()
                    and all(part not in ('', '.', '..') for part in parts)
                    and parts[0] in allowed_roots, 'Nieprawidłowa ścieżka w ZIP')
            require(name not in seen, 'Powtórzona ścieżka w ZIP')
            seen.add(name)
            require(mode in (0, stat.S_IFREG, stat.S_IFDIR),
                    'Dowiązanie lub plik specjalny w ZIP')
            require(not (entry.flag_bits & 1), 'Zaszyfrowany ZIP')
            expanded += entry.file_size
            require(entry.file_size <= 4 * 1024**3 and expanded <= 12 * 1024**3,
                    'Przekroczony limit rozpakowanego ZIP')
        require('manifest.toml' in seen, 'Brak manifestu w korzeniu artefaktu')
        destination.mkdir(parents=True, exist_ok=False)
        for entry in entries:
            path = destination / entry.filename
            if entry.is_dir():
                path.mkdir(parents=True, exist_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(entry) as source_file, path.open('xb') as output:
                    shutil.copyfileobj(source_file, output)


def check_payload(spec, destination):
    with (destination / 'manifest.toml').open('rb') as source:
        manifest = tomllib.load(source)
    require(manifest['platform'] == spec['platform'], 'Nieprawidłowa platforma manifestu')
    backend = 'vulkan' if spec['backend'] == 'vulkan' else 'cuda'
    candidates = list((destination / 'lib-static' / 'llama-cpp').rglob(f'libggml-{backend}.a'))
    require(candidates and all(path.is_file() and path.stat().st_size > 0 for path in candidates),
            'Brak wymaganej biblioteki backendu')


def main():
    parser = argparse.ArgumentParser(description='Odzyskanie bibliotek natywnych beta.1')
    parser.add_argument('variant', choices=SPECS)
    args = parser.parse_args()
    require(os.environ.get('GITHUB_REPOSITORY') == REPOSITORY, 'Nieprawidłowe repozytorium')
    check_recipes()
    spec = SPECS[args.variant]
    run = api(f'actions/runs/{spec["run_id"]}')
    job = api(f'actions/jobs/{spec["job_id"]}')
    artifact = api(f'actions/artifacts/{spec["id"]}')
    check_metadata(spec, run, job, artifact)
    output = Path('recovered')
    require(not output.exists(), 'Katalog wynikowy już istnieje')
    with tempfile.TemporaryDirectory() as scratch:
        archive = Path(scratch) / 'native.zip'
        download(spec, archive)
        extract(archive, output / 'native')
    check_payload(spec, output / 'native')
    provenance = dict(spec, application_source_sha=SOURCE_SHA,
                      native_recipe_diff_sha256=RECIPE_DIFF_SHA256,
                      expires_at=artifact['expires_at'], source_job_attempt=job['run_attempt'],
                      recovery_sha=os.environ['GITHUB_SHA'], recovery_run_id=os.environ['GITHUB_RUN_ID'])
    (output / 'provenance.json').write_text(json.dumps(provenance, indent=2) + '\n')
    print(f'Odzyskano {spec["name"]}; digest i pochodzenie potwierdzone')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Nie wypisujemy obiektów sieciowych zawierających podpisane adresy URL.
        print(f'Odzyskiwanie bibliotek nie powiodło się: {type(error).__name__}', file=sys.stderr)
        if isinstance(error, (ValueError, RuntimeError)):
            print(str(error), file=sys.stderr)
        sys.exit(1)
