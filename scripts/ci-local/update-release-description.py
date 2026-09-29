#!/usr/bin/env python3
# ============ File: update-release-description.py — Apply the reviewed English release description. ============
import hashlib
import json
import os
from urllib.error import HTTPError
from urllib.request import HTTPRedirectHandler, Request, build_opener

REPOSITORY = 'Slyb00ts/TentaFlow'
RELEASE_ID = 399110669
TAG = 'v0.4.0-beta.1'
OLD_BODY_SHA256 = '6b03cf765619e46e3442dd56d6a62483dce01b0a06a6cd2af315ad77d5ba52cd'
NEW_BODY_SHA256 = 'df33c94f3522fdbe7925bb637250bb4eb3ec0137976f4850e4791438830456d8'
EXPECTED_SNAPSHOT_SHA256 = 'acd734f2b23f3cb047da49a8551efd474d9de7f3ed6eb3e99988a90f82de1ab0'
METADATA_FIELDS = ('id', 'tag_name', 'target_commitish', 'name', 'draft', 'prerelease',
                   'created_at', 'published_at', 'immutable')
ASSET_FIELDS = ('id', 'name', 'digest', 'size', 'state')
PROVENANCE_LINK = ('\n\nBuild provenance: [release-provenance.json]'
                   f'(https://github.com/{REPOSITORY}/releases/download/'
                   f'{TAG}/release-provenance.json).\n')


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, url):
        return None


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def api(method, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    request = Request(f'https://api.github.com/repos/{REPOSITORY}/{path}', data=data,
                      method=method, headers={
                          'Authorization': f'Bearer {os.environ["GITHUB_TOKEN"]}',
                          'Accept': 'application/vnd.github+json',
                          'Content-Type': 'application/json',
                          'X-GitHub-Api-Version': '2022-11-28',
                          'User-Agent': 'tentaflow-release-description'})
    try:
        with build_opener(NoRedirect).open(request, timeout=120) as response:
            require(response.status == 200, 'Unexpected GitHub API status')
            return json.load(response)
    except HTTPError as error:
        raise RuntimeError(f'GitHub API returned HTTP {error.code}') from None


def snapshot(release):
    assets = api('GET', f'releases/{RELEASE_ID}/assets?per_page=100')
    require(len(assets) == 31, 'Release asset count changed')
    return {'metadata': {key: release[key] for key in METADATA_FIELDS},
            'assets': sorted([{key: asset[key] for key in ASSET_FIELDS} for asset in assets],
                             key=lambda asset: asset['id'])}


def update():
    require(os.environ.get('GITHUB_REPOSITORY') == REPOSITORY, 'Unexpected repository')
    path = f'releases/{RELEASE_ID}'
    release = api('GET', path)
    require(release['id'] == RELEASE_ID and release['tag_name'] == TAG
            and release['draft'] is False and release['prerelease'] is True,
            'Unexpected release identity or publication state')
    require(digest(release['body']) == OLD_BODY_SHA256, 'Release body changed; refusing update')
    before = snapshot(release)
    require(digest(json.dumps(before, sort_keys=True)) == EXPECTED_SNAPSHOT_SHA256,
            'Release metadata or assets changed; refusing update')
    body = release['body'].split('\n\n## Pochodzenie kompilacji', 1)[0] + PROVENANCE_LINK
    require(digest(body) == NEW_BODY_SHA256, 'Unexpected replacement description')
    response = api('PATCH', path, {'body': body})
    require(response['body'] == body, 'GitHub did not accept the exact description')
    after = api('GET', path)
    require(after['body'] == body and snapshot(after) == before,
            'Release verification failed after description update')
    print('English release description published; all 31 assets and release metadata unchanged.')


if __name__ == '__main__':
    try:
        update()
    except Exception as error:
        print(f'Release description update failed ({type(error).__name__}).')
        raise SystemExit(1) from None
