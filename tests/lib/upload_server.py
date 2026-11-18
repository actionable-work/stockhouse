# A local fake of the Stockhouse upload and lease protocol v1 for the VM
# stories, written from this repository's spec. It is a test fixture, not a
# server implementation, and claims nothing about any real server.
#
# State lives under /var/lib/stockhouse-fake so it survives reboots:
#   state.json     uploads (declaration and state) and held store paths
#   requests.log   one line per request: METHOD REL HASH RESULT
#   leases.log     one line per lease: MACHINE SLOT STORE_PATH
#   control/       quota, down, present-<hash>, reject-<hash>,
#                  stall-<hash>, drop-<hash>
import hashlib
import http.server
import json
import os
import sys
import threading
import time

ROOT = "/var/lib/stockhouse-fake"
CONTROL = f"{ROOT}/control"
TOKEN_FILE = sys.argv[1]
PREFIX = "/t/1/"
ALPHABET = "0123456789abcdfghijklmnpqrsvwxyz"
LOCK = threading.Lock()


def nix32(digest):
    out = []
    for n in range(len(digest) * 8 // 5, -1, -1):
        b = n * 5
        i, j = b // 8, b % 8
        c = (digest[i] >> j) | ((digest[i + 1] << (8 - j)) if i + 1 < len(digest) else 0)
        out.append(ALPHABET[c & 0x1F])
    return "".join(out)


def load():
    try:
        with open(f"{ROOT}/state.json") as f:
            return json.load(f)
    except FileNotFoundError:
        return {"uploads": {}, "held": []}


def save(state):
    tmp = f"{ROOT}/state.json.tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, f"{ROOT}/state.json")


def log(line, name="requests.log"):
    with open(f"{ROOT}/{name}", "a") as f:
        f.write(line + "\n")


def control(name):
    return os.path.exists(f"{CONTROL}/{name}")


def store_hash(path):
    return path.rsplit("/", 1)[-1][:32]


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def reply(self, code, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)
        self.close_connection = True

    def authorized(self):
        with open(TOKEN_FILE) as f:
            token = f.read().strip()
        return self.headers.get("Authorization") == f"Bearer {token}"

    def body(self):
        length = int(self.headers.get("Content-Length", "0"))
        data = b""
        while len(data) < length:
            chunk = self.rfile.read(min(65536, length - len(data)))
            if not chunk:
                break
            data += chunk
        return data, len(data) == length

    def route(self):
        rel = self.path[len(PREFIX):] if self.path.startswith(PREFIX) else None
        if rel == "nix-cache-info" and self.command == "GET":
            data = b"StoreDir: /nix/store\n"
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return None
        if rel is not None and rel.endswith(".narinfo"):
            self.reply(404)
            return None
        if not self.authorized():
            log(f"{self.command} {rel} - 401")
            self.reply(401)
            return None
        if control("down"):
            log(f"{self.command} {rel} - 503")
            self.reply(503, {"error": "unavailable"})
            return None
        return rel.split("/") if rel else None

    def do_GET(self):
        parts = self.route()
        if parts is None:
            return
        if len(parts) == 2 and parts[0] == "uploads":
            with LOCK:
                upload = load()["uploads"].get(parts[1])
            if upload is None:
                log(f"GET uploads/{parts[1]} - unknown_upload")
                return self.reply(404, {"error": "unknown_upload"})
            log(f"GET uploads/{parts[1]} {store_hash(upload['decl']['store_path'])} {upload['state']}")
            return self.reply(200, {"state": upload["state"]})
        self.reply(404)

    def do_POST(self):
        parts = self.route()
        if parts is None:
            return
        data, _ = self.body()
        if len(parts) != 2 or parts[0] != "uploads":
            return self.reply(404)
        upload_id = parts[1]
        try:
            decl = json.loads(data)
            path = decl["store_path"]
        except (ValueError, KeyError):
            log(f"POST uploads/{upload_id} - bad_request")
            return self.reply(400, {"error": "bad_request"})
        h = store_hash(path)
        base = path.rsplit("/", 1)[-1]
        with LOCK:
            state = load()
            current = state["uploads"].get(upload_id)
            if current and current["state"] == "published":
                result, body = "published", {"state": "published"}
            elif base in state["held"] or control(f"present-{h}"):
                result, body = "present", {"state": "present"}
            elif control("quota"):
                result, body = "quota_exceeded", None
            else:
                missing = [
                    r
                    for r in decl["references"]
                    if r != base and r not in state["held"] and not control(f"present-{r[:32]}")
                ]
                if missing:
                    result, body = "missing_references", {"error": "missing_references", "missing": missing}
                else:
                    state["uploads"].setdefault(upload_id, {"decl": decl, "state": "open"})
                    save(state)
                    result, body = "open", {"state": "open"}
        log(f"POST uploads/{upload_id} {h} {result}")
        if result == "quota_exceeded":
            return self.reply(507, {"error": "quota_exceeded"})
        if result == "missing_references":
            return self.reply(422, body)
        self.reply(200, body)

    def do_PUT(self):
        parts = self.route()
        if parts is None:
            return
        if len(parts) == 3 and parts[0] == "leases":
            data, _ = self.body()
            path = json.loads(data)["store_path"]
            log(f"{parts[1]} {parts[2]} {path}", "leases.log")
            log(f"PUT leases/{parts[1]}/{parts[2]} - renewed")
            return self.reply(200, {"state": "renewed"})
        if len(parts) != 3 or parts[0] != "uploads" or parts[2] != "nar":
            return self.reply(404)
        upload_id = parts[1]
        with LOCK:
            upload = load()["uploads"].get(upload_id)
        if upload is None:
            self.body()
            log(f"PUT uploads/{upload_id}/nar - unknown_upload")
            return self.reply(404, {"error": "unknown_upload"})
        decl = upload["decl"]
        h = store_hash(decl["store_path"])
        if control(f"stall-{h}"):
            log(f"PUT uploads/{upload_id}/nar {h} stalled")
            while control(f"stall-{h}"):
                time.sleep(0.2)
        data, complete = self.body()
        if not complete:
            log(f"PUT uploads/{upload_id}/nar {h} aborted")
            self.close_connection = True
            return
        if control(f"reject-{h}"):
            log(f"PUT uploads/{upload_id}/nar {h} verification_failed")
            return self.reply(400, {"error": "verification_failed"})
        digest = "sha256:" + nix32(hashlib.sha256(data).digest())
        if digest != decl["nar_hash"] or len(data) != decl["nar_size"]:
            log(f"PUT uploads/{upload_id}/nar {h} verification_failed")
            return self.reply(400, {"error": "verification_failed"})
        with LOCK:
            state = load()
            state["uploads"][upload_id]["state"] = "published"
            base = decl["store_path"].rsplit("/", 1)[-1]
            if base not in state["held"]:
                state["held"].append(base)
            save(state)
        if control(f"drop-{h}"):
            os.remove(f"{CONTROL}/drop-{h}")
            log(f"PUT uploads/{upload_id}/nar {h} dropped")
            self.close_connection = True
            return
        log(f"PUT uploads/{upload_id}/nar {h} published")
        self.reply(200, {"state": "published"})

    def log_message(self, *args):
        pass


os.makedirs(CONTROL, exist_ok=True)
http.server.ThreadingHTTPServer(("127.0.0.1", 8486), Handler).serve_forever()
