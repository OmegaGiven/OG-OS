#!/usr/bin/env python3
"""App Store Connect chores for the automatic Mac App Store upload
(appstore.yml). Never submits for review.

  asc.py ensure-version 0.2.1
      make sure the macOS version being prepared is 0.2.1: renames the
      version still in "Prepare for Submission" (or similar editable
      state), or creates a new one, which copies the previous listing
  asc.py attach-build 0.2.1 [--wait MINUTES]
      wait until the newest build of 0.2.1 has finished processing, then
      attach it to that version

Needs ASC_KEY (the .p8's text), ASC_KEY_ID and ASC_ISSUER; pip install
pyjwt cryptography."""

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

import jwt  # PyJWT

BUNDLE = os.environ.get("ASC_BUNDLE", "com.omegagiven.ogfiles")
API = "https://api.appstoreconnect.apple.com"
PLATFORM = "MAC_OS"
# states in which a version's string can still change and a build can be set
EDITABLE = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED",
            "METADATA_REJECTED", "INVALID_BINARY"}


def token():
    now = int(time.time())
    return jwt.encode(
        {"iss": os.environ["ASC_ISSUER"], "iat": now, "exp": now + 1000, "aud": "appstoreconnect-v1"},
        os.environ["ASC_KEY"],
        algorithm="ES256",
        headers={"kid": os.environ["ASC_KEY_ID"], "typ": "JWT"},
    )


def call(method, path, body=None, **params):
    url = API + path + ("?" + urllib.parse.urlencode(params) if params else "")
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={
        "Authorization": f"Bearer {token()}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            raw = r.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        sys.exit(f"{method} {path}: {e.code} {e.read().decode()[:800]}")


def app_id():
    apps = call("GET", "/v1/apps", **{"filter[bundleId]": BUNDLE})["data"]
    if not apps:
        sys.exit(f"no app with bundle id {BUNDLE} in App Store Connect")
    return apps[0]["id"]


def mac_versions(aid):
    vs = call("GET", f"/v1/apps/{aid}/appStoreVersions", limit=50,
              **{"filter[platform]": PLATFORM})["data"]
    return vs


def ensure_version(ver):
    aid = app_id()
    vs = mac_versions(aid)
    for v in vs:
        if v["attributes"]["versionString"] == ver:
            print(f"version {ver} exists ({v['attributes']['appStoreState']})")
            return
    editable = [v for v in vs if v["attributes"]["appStoreState"] in EDITABLE]
    if editable:
        v = editable[0]
        old = v["attributes"]["versionString"]
        call("PATCH", f"/v1/appStoreVersions/{v['id']}", {"data": {
            "type": "appStoreVersions", "id": v["id"], "attributes": {"versionString": ver}}})
        print(f"renamed version {old} -> {ver}")
        return
    call("POST", "/v1/appStoreVersions", {"data": {
        "type": "appStoreVersions",
        "attributes": {"platform": PLATFORM, "versionString": ver},
        "relationships": {"app": {"data": {"type": "apps", "id": aid}}}}})
    print(f"created version {ver}")


def attach_build(ver, wait_min):
    aid = app_id()
    deadline = time.time() + wait_min * 60
    while True:
        builds = call("GET", "/v1/builds", **{
            "filter[app]": aid, "filter[preReleaseVersion.version]": ver,
            "filter[preReleaseVersion.platform]": PLATFORM,
            "sort": "-uploadedDate", "limit": 1})["data"]
        state = builds[0]["attributes"]["processingState"] if builds else "NOT_YET_VISIBLE"
        if state == "VALID":
            break
        if state in ("FAILED", "INVALID"):
            sys.exit(f"build of {ver} {state}")
        if time.time() > deadline:
            print(f"::warning::build of {ver} still {state} after {wait_min} min; attach it in App Store Connect")
            return
        print(f"build of {ver}: {state}, waiting…")
        time.sleep(60)
    b = builds[0]
    v = next((v for v in mac_versions(aid) if v["attributes"]["versionString"] == ver), None)
    if v is None:
        sys.exit(f"no App Store version {ver}; run ensure-version first")
    if v["attributes"]["appStoreState"] not in EDITABLE:
        print(f"version {ver} is {v['attributes']['appStoreState']}; not changing its build")
        return
    call("PATCH", f"/v1/appStoreVersions/{v['id']}/relationships/build",
         {"data": {"type": "builds", "id": b["id"]}})
    print(f"attached build {b['attributes']['version']} to version {ver}")


def main(argv):
    if len(argv) >= 2 and argv[0] == "ensure-version":
        ensure_version(argv[1])
    elif len(argv) >= 2 and argv[0] == "attach-build":
        wait = int(argv[argv.index("--wait") + 1]) if "--wait" in argv else 45
        attach_build(argv[1], wait)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
