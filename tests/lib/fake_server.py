"""Fake OAuth 2.0 server and private cache for the sign-in VM stories.

Test code only. It implements the public RFC 6749, 7636, 7009 and 8628
behavior the client relies on and a bearer-protected Nix cache under
/t/1/. It is not a model of any real server; every token is random per run.
Control routes under /__fake/ approve device codes, end the grant family,
set the access token lifetime and report state.
"""

import base64
import hashlib
import http.server
import json
import os
import secrets
import threading
import time
import urllib.parse

CACHE = "/var/lib/stockhouse-vm/server-cache"
LOCK = threading.Lock()
STATE = {
    "lifetime": 600,
    "access": None,
    "access_expires": 0,
    "refresh": None,
    "rotated": [],
    "issued": [],
    "reuse": [],
    "overlaps": [],
    "revoked": [],
    "refreshes": 0,
    "codes": {},
    "devices": {},
    "in_flight": {},
}


def token(prefix):
    return f"{prefix}-{secrets.token_urlsafe(18)}"


def issue():
    access, refresh = token("vm-access"), token("vm-refresh")
    if STATE["refresh"]:
        STATE["rotated"].append(STATE["refresh"])
    STATE["access"], STATE["refresh"] = access, refresh
    STATE["access_expires"] = time.time() + STATE["lifetime"]
    STATE["issued"] += [access, refresh]
    return {
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": STATE["lifetime"],
        "refresh_token": refresh,
        "scope": "cache:read",
    }


def end_family():
    if STATE["refresh"]:
        STATE["rotated"].append(STATE["refresh"])
    STATE["access"] = STATE["refresh"] = None


def oauth_error(code):
    return 400, {"error": code}


def refresh_grant(form):
    sent = form.get("refresh_token", "")
    with LOCK:
        STATE["in_flight"][sent] = STATE["in_flight"].get(sent, 0) + 1
        if STATE["in_flight"][sent] > 1:
            STATE["overlaps"].append(sent)
    time.sleep(0.2)
    with LOCK:
        STATE["in_flight"][sent] -= 1
        STATE["refreshes"] += 1
        if sent and sent == STATE["refresh"]:
            return 200, issue()
        if sent in STATE["rotated"]:
            STATE["reuse"].append(sent)
            end_family()
        return oauth_error("invalid_grant")


def code_grant(form):
    with LOCK:
        entry = STATE["codes"].pop(form.get("code", ""), None)
        if entry is None or form.get("redirect_uri") != entry["redirect"]:
            return oauth_error("invalid_grant")
        verifier = form.get("code_verifier", "").encode()
        digest = base64.urlsafe_b64encode(hashlib.sha256(verifier).digest()).rstrip(b"=").decode()
        if digest != entry["challenge"]:
            return oauth_error("invalid_grant")
        return 200, issue()


def device_grant(form):
    with LOCK:
        device = STATE["devices"].get(form.get("device_code", ""))
        if device is None:
            return oauth_error("expired_token")
        if not device["approved"]:
            return oauth_error("authorization_pending")
        del STATE["devices"][form["device_code"]]
        return 200, issue()


GRANTS = {
    "refresh_token": refresh_grant,
    "authorization_code": code_grant,
    "urn:ietf:params:oauth:grant-type:device_code": device_grant,
}


def device_authorization(_form):
    with LOCK:
        code = token("vm-device")
        user = f"VM-{len(STATE['devices']) + 1:04d}"
        STATE["devices"][code] = {"user": user, "approved": False}
    uri = "http://127.0.0.1:8486/device"
    return 200, {
        "device_code": code,
        "user_code": user,
        "verification_uri": uri,
        "expires_in": 600,
        "interval": 1,
    }


def revoke(form):
    with LOCK:
        STATE["revoked"].append(form.get("token", ""))
        if form.get("token") == STATE["refresh"]:
            end_family()
    return 200, {}


def control(path, query):
    with LOCK:
        if path == "/__fake/approve":
            for device in STATE["devices"].values():
                if device["user"] == query.get("user_code"):
                    device["approved"] = True
        elif path == "/__fake/revoke":
            end_family()
        elif path == "/__fake/config":
            STATE["lifetime"] = int(query["lifetime"])
        state = {k: v for k, v in STATE.items() if k not in ("codes", "devices", "in_flight")}
        return 200, json.loads(json.dumps(state))


class Handler(http.server.BaseHTTPRequestHandler):
    def reply(self, status, body, content_type="application/json", headers=()):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        form = dict(urllib.parse.parse_qsl(self.rfile.read(length).decode()))
        if self.path == "/oauth/token":
            grant = GRANTS.get(form.get("grant_type", ""))
            status, body = grant(form) if grant else oauth_error("unsupported_grant_type")
        elif self.path == "/oauth/device":
            status, body = device_authorization(form)
        elif self.path == "/oauth/revoke":
            status, body = revoke(form)
        else:
            status, body = 404, {}
        self.reply(status, body)

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        if url.path.startswith("/__fake/"):
            self.reply(*control(url.path, query))
        elif url.path == "/oauth/authorize":
            self.authorize(query)
        elif url.path.startswith("/t/1/"):
            self.cache(url.path[len("/t/1/"):])
        else:
            self.reply(404, {})

    def authorize(self, query):
        if query.get("code_challenge_method") != "S256" or "redirect_uri" not in query:
            return self.reply(400, {})
        with LOCK:
            code = token("vm-code")
            STATE["codes"][code] = {
                "challenge": query.get("code_challenge", ""),
                "redirect": query["redirect_uri"],
            }
        params = urllib.parse.urlencode({"code": code, "state": query.get("state", "")})
        self.reply(302, b"", headers=[("Location", f"{query['redirect_uri']}?{params}")])

    def cache(self, rel):
        with LOCK:
            current = STATE["access"]
            expired = time.time() > STATE["access_expires"]
        # Only the current, unexpired access token is accepted.
        if current is None or expired or self.headers.get("Authorization") != f"Bearer {current}":
            return self.reply(401, b"", "text/plain")
        if rel == "nix-cache-info":
            return self.reply(200, b"StoreDir: /nix/store\n", "text/x-nix-cache-info")
        path = os.path.normpath(os.path.join(CACHE, rel))
        if not path.startswith(CACHE + "/") or not os.path.isfile(path):
            return self.reply(404, b"", "text/plain")
        with open(path, "rb") as f:
            self.reply(200, f.read(), "application/octet-stream")

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", 8486), Handler).serve_forever()
