# Loopback VM fixture for the upload and lease protocols.
# State and control files live under /var/lib/stockhouse-fake across reboots.
import hashlib
import http.server
import json
import os
import re
import sys
import threading
import time
import unicodedata

ROOT = "/var/lib/stockhouse-fake"
CONTROL = f"{ROOT}/control"
TOKEN_FILE = sys.argv[1]
PREFIX = "/t/1/"
ALPHABET = "0123456789abcdfghijklmnpqrsvwxyz"
LOCK = threading.Lock()
ACTIVE_NARS = 0
PEAK_NARS = 0
KIB = 1024
BATCH_LIMITS = {"declare": 8 * KIB * KIB, "status": 64 * KIB, "publish": 64 * KIB, "leases": 64 * KIB}
DECL_FIELDS = {"store_path", "nar_hash", "nar_size", "references", "deriver", "ca"}


def nix32(digest):
    value = int.from_bytes(digest, "little")
    return "".join(ALPHABET[(value >> shift) & 31] for shift in range(255, -1, -5))


def load():
    try:
        with open(f"{ROOT}/state.json") as f:
            return json.load(f)
    except FileNotFoundError:
        return {"uploads": {}, "held": {}, "leases": {}}


def save(state):
    tmp = f"{ROOT}/state.json.tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, f"{ROOT}/state.json")
    directory = os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def log(line, name="requests.log"):
    with open(f"{ROOT}/{name}", "a") as f:
        f.write(line + "\n")


def control(name):
    return os.path.exists(f"{CONTROL}/{name}")


def basename(path):
    return path.rsplit("/", 1)[-1]


def matches(pattern, text):
    return isinstance(text, str) and re.fullmatch(pattern, text) is not None


def valid_base(text):
    return matches(f"[{ALPHABET}]{{32}}-(?!\\.)[A-Za-z0-9+._?=-]{{1,211}}", text)


def valid_path(path):
    return isinstance(path, str) and path.startswith("/nix/store/") and valid_base(path[11:])


def valid_id(text):
    return matches(r"[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}", text)


def strict_object(pairs):
    result = dict(pairs)
    if len(result) != len(pairs):
        raise ValueError("duplicate key")
    return result


def strict_json(data):
    def invalid_constant(_):
        raise ValueError("non-JSON number")

    return json.loads(data.decode("utf-8"), object_pairs_hook=strict_object, parse_constant=invalid_constant)


def declaration(row):
    if not valid_path(row["store_path"]):
        raise ValueError("store path")
    nar_hash = row["nar_hash"]
    if matches(r"sha256:[0-9a-f]{64}", nar_hash):
        row["nar_hash"] = "sha256:" + nix32(bytes.fromhex(nar_hash[7:]))
    elif not matches(f"sha256:[01][{ALPHABET}]{{51}}", nar_hash):
        raise ValueError("nar hash")
    size = row["nar_size"]
    if type(size) is not int or not 0 < size < 2**64:
        raise ValueError("nar size")
    refs = row["references"]
    if not isinstance(refs, list) or len(refs) > 10_000 or not all(map(valid_base, refs)):
        raise ValueError("references")
    if len(set(refs)) != len(refs):
        raise ValueError("duplicate reference")
    deriver = row["deriver"]
    if deriver is not None and (not valid_base(deriver) or not deriver.endswith(".drv")):
        raise ValueError("deriver")
    ca = row["ca"]
    if ca is not None and (
        not isinstance(ca, str) or len(ca.encode()) > 1024 or any(unicodedata.category(c) == "Cc" for c in ca)
    ):
        raise ValueError("content address")
    return row


def distinct(items):
    if len(items) != len(set(items)):
        raise ValueError("duplicate entry")


def parse_batch(kind, data):
    doc = strict_json(data)
    collection = "leases" if kind == "leases" else "uploads"
    if not isinstance(doc, dict) or doc.keys() != {"version", collection}:
        raise ValueError("batch shape")
    entries = doc[collection]
    limit = 64 if kind == "leases" else 1000
    if type(doc["version"]) is not int or doc["version"] != 2 or not isinstance(entries, list) or not 1 <= len(entries) <= limit:
        raise ValueError("batch header")
    if kind == "leases":
        for row in entries:
            if (
                not isinstance(row, dict) or row.keys() != {"machine", "slot", "store_path"}
                or not matches(r"[0-9a-f]{32}", row["machine"]) or type(row["slot"]) is not int
                or not 1 <= row["slot"] <= 10 or not valid_path(row["store_path"])
            ):
                raise ValueError("lease")
        distinct([(row["machine"], row["slot"]) for row in entries])
    elif kind == "declare":
        for row in entries:
            if not isinstance(row, dict) or row.keys() != DECL_FIELDS | {"id"} or not valid_id(row["id"]):
                raise ValueError("declaration")
            declaration(row)
        distinct([row["id"] for row in entries])
        distinct([row["store_path"] for row in entries])
    else:
        if not all(map(valid_id, entries)):
            raise ValueError("upload ID")
        distinct(entries)
    return entries


def held(state, base):
    return base in state["held"] or control(f"present-{base[:32]}")


def quota(state, decl):
    if control("quota"):
        return True
    try:
        with open(f"{CONTROL}/quota-bytes") as f:
            limit = int(f.read())
    except FileNotFoundError:
        return False
    used = sum(item["nar_size"] for item in state["held"].values())
    base = basename(decl["store_path"])
    return used + (0 if base in state["held"] else decl["nar_size"]) > limit


def current_upload(state, upload_id):
    return None if control(f"forget-{upload_id}") else state["uploads"].get(upload_id)


def current_result(state, upload, decl):
    if upload and upload["state"] == "published":
        return {"state": "published"}
    base = basename(decl["store_path"])
    published = state["held"].get(base)
    if published and published["nar_hash"] != decl["nar_hash"]:
        return {"error": "conflict"}
    if held(state, base):
        return {"state": "present"}
    if upload and upload["state"] == "verification_failed":
        return {"error": "verification_failed"}
    return {"state": upload["state"] if upload else "open"}


def missing_refs(state, decl):
    base = basename(decl["store_path"])
    return [ref for ref in decl["references"] if ref != base and not held(state, ref)]


def declare(state, upload_id, decl, *, v2):
    if control(f"forget-{upload_id}"):
        return {"error": "unknown_upload"}
    upload = state["uploads"].get(upload_id)
    if upload and upload["decl"] != decl:
        return {"error": "conflict"}
    result = current_result(state, upload, decl)
    if result.get("state") not in ("open", "verified"):
        return result
    if upload is not None:
        return result if v2 else {"state": "open"}
    if quota(state, decl):
        return {"error": "quota_exceeded"}
    missing = [] if v2 else missing_refs(state, decl)
    if missing:
        return {"error": "missing_references", "missing": missing}
    state["uploads"][upload_id] = {"decl": decl, "state": "open"}
    return {"state": "open"}


def status(state, upload_id, *, v2):
    upload = current_upload(state, upload_id)
    if upload is None:
        return {"error": "unknown_upload"}
    result = current_result(state, upload, upload["decl"])
    if result.get("error") == "conflict":
        known = upload["state"]
        result = {"error": known} if known == "verification_failed" else {"state": known}
    if not v2 and result.get("state") == "verified":
        return {"state": "open"}
    return result


def publish(state, upload_id):
    upload = current_upload(state, upload_id)
    if upload is None:
        return {"error": "unknown_upload"}
    decl = upload["decl"]
    result = current_result(state, upload, decl)
    if result.get("state") != "verified":
        return result
    missing = missing_refs(state, decl)
    if missing:
        return {"error": "missing_references", "missing": missing}
    if quota(state, decl):
        return {"error": "quota_exceeded"}
    upload["state"] = "published"
    state["held"][basename(decl["store_path"])] = decl
    return {"state": "published"}


def renew(state, row):
    machine, slot, path = row["machine"], row["slot"], row["store_path"]
    if control("leases-full"):
        return {"error": "machines_full"}
    if control("lease-missing"):
        return {"error": "unknown_path"}
    state["leases"][f"{machine}/{slot}"] = {"store_path": path, "expires_at": time.time() + 3600}
    log(f"{machine} {slot} {path}", "leases.log")
    return {"state": "renewed"}


def result_name(result):
    return result.get("state", result.get("error"))


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def reply(self, code, body=None, retry_after=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        if retry_after is not None:
            self.send_header("Retry-After", str(retry_after))
        self.end_headers()
        self.wfile.write(data)
        self.close_connection = True

    def error(self, code, name):
        self.reply(code, {"error": name})

    def length(self):
        return int(self.headers.get("Content-Length", "0"))

    def body(self):
        length = self.length()
        data = bytearray()
        while len(data) < length:
            chunk = self.rfile.read(min(65536, length - len(data)))
            if not chunk:
                break
            data.extend(chunk)
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
        if not rel or rel.endswith(".narinfo"):
            self.reply(404)
            return None
        with open(TOKEN_FILE) as f:
            token = f.read().strip()
        if self.headers.get("Authorization") != f"Bearer {token}":
            log(f"{self.command} {rel} - 401")
            self.reply(401)
            return None
        if control("down"):
            log(f"{self.command} {rel} - 503")
            self.error(503, "unavailable")
            return None
        if control("v1-only") and (rel == "protocol" or rel.startswith("v2/")):
            log(f"{self.command} {rel} - 404")
            self.reply(404)
            return None
        return rel.split("/")

    def do_GET(self):
        parts = self.route()
        if parts is None:
            return
        if parts == ["protocol"]:
            log("GET protocol - 200")
            return self.reply(200, {"versions": [1, 2]})
        if len(parts) == 2 and parts[0] == "uploads":
            upload_id = parts[1]
            with LOCK:
                state = load()
                upload = current_upload(state, upload_id)
                result = status(state, upload_id, v2=False)
            h = basename(upload["decl"]["store_path"])[:32] if upload else "-"
            log(f"GET uploads/{upload_id} {h} {result_name(result)}")
            return self.v1_reply(result)
        self.reply(404)

    def v1_reply(self, result):
        code = {
            "unknown_upload": 404, "conflict": 409, "verification_failed": 400, "too_large": 413,
            "missing_references": 422, "quota_exceeded": 507, "machines_full": 503, "unknown_path": 422,
        }.get(result.get("error"), 200)
        self.reply(code, result)

    def do_POST(self):
        parts = self.route()
        if parts is None:
            return
        if len(parts) != 2:
            return self.reply(404)
        kind, name = parts
        if kind == "v2" and name in BATCH_LIMITS:
            return self.batch(name)
        if kind != "uploads":
            return self.reply(404)
        try:
            data, complete = self.body()
            row = strict_json(data)
            if not complete or not isinstance(row, dict) or row.keys() != DECL_FIELDS | {"version"} or row.pop("version") != 1:
                raise ValueError("declare")
            decl = declaration(row)
        except (ValueError, TypeError, KeyError):
            log(f"POST uploads/{name} - bad_request")
            return self.error(400, "bad_request")
        with LOCK:
            state = load()
            result = declare(state, name, decl, v2=False)
            save(state)
        log(f"POST uploads/{name} {basename(decl['store_path'])[:32]} {result_name(result)}")
        self.v1_reply(result)

    def batch(self, kind):
        rel = f"v2/{kind}"
        with LOCK:
            busy = control("busy-once")
            if busy:
                os.remove(f"{CONTROL}/busy-once")
        if self.length() > BATCH_LIMITS[kind]:
            log(f"POST {rel} - too_large")
            return self.error(413, "too_large")
        try:
            data, complete = self.body()
            if not complete:
                raise ValueError("partial body")
            entries = parse_batch(kind, data)
        except (ValueError, TypeError, KeyError):
            log(f"POST {rel} - bad_request")
            return self.error(400, "bad_request")
        self.log_busy(rel, data)
        if busy:
            log(f"POST {rel} - 429")
            return self.reply(429, retry_after=1)
        if kind == "publish" and control("hold-publish"):
            log(f"POST {rel} - 503")
            return self.reply(503)
        with LOCK:
            state = load()
            log(f"POST {rel} - entries={len(entries)}")
            results = self.batch_results(kind, state, entries)
            if kind != "status":
                save(state)
        if kind == "publish" and control("drop-publish"):
            self.close_connection = True
            return
        self.reply(200, {"results": results})

    def log_busy(self, rel, data):
        if control("trace-busy"):
            log(f"{rel} {time.monotonic()} {hashlib.sha256(data).hexdigest()}", "busy.log")

    def batch_results(self, kind, state, entries):
        names_left = 10_000
        results = []
        for index, row in enumerate(entries):
            result = self.batch_entry(kind, state, row)
            if "missing" in result:
                # Leave one name for every remaining entry that could fail.
                reserve = len(entries) - index - 1
                result["missing"] = result["missing"][:names_left - reserve]
                names_left -= len(result["missing"])
            results.append(result)
        return results

    def batch_entry(self, kind, state, row):
        if kind == "leases":
            result = renew(state, row)
            log(f"ENTRY v2/leases {row['slot']} {basename(row['store_path'])[:32]} {result_name(result)}")
            return {"machine": row["machine"], "slot": row["slot"], **result}
        if kind == "declare":
            upload_id = row["id"]
            decl = {key: row[key] for key in DECL_FIELDS}
            result = declare(state, upload_id, decl, v2=True)
        else:
            upload_id = row
            upload = current_upload(state, upload_id)
            decl = upload["decl"] if upload else None
            result = publish(state, upload_id) if kind == "publish" else status(state, upload_id, v2=True)
        h = basename(decl["store_path"])[:32] if decl else "-"
        log(f"ENTRY v2/{kind} {upload_id} {h} {result_name(result)}")
        return {"id": upload_id, **result}

    def do_PUT(self):
        parts = self.route()
        if parts is None:
            return
        if len(parts) == 3 and parts[0] == "leases":
            data, _ = self.body()
            path = json.loads(data)["store_path"]
            with LOCK:
                state = load()
                result = renew(state, {"machine": parts[1], "slot": int(parts[2]), "store_path": path})
                save(state)
            log(f"PUT leases/{parts[1]}/{parts[2]} - {result_name(result)}")
            return self.v1_reply(result)
        if len(parts) == 3 and parts[0] == "uploads" and parts[2] == "nar":
            return self.nar(parts[1], v2=False)
        if len(parts) == 3 and parts[:2] == ["v2", "nar"]:
            return self.nar(parts[2], v2=True)
        self.reply(404)

    def nar(self, upload_id, *, v2):
        rel = f"v2/nar/{upload_id}" if v2 else f"uploads/{upload_id}/nar"
        with LOCK:
            state = load()
            upload = current_upload(state, upload_id)
        if upload is None:
            self.body()
            log(f"PUT {rel} - unknown_upload")
            return self.error(404, "unknown_upload")
        decl = upload["decl"]
        h = basename(decl["store_path"])[:32]
        if v2 and control("trace-nar-start"):
            log(f"START {rel} {h} reading")
        data, complete = self.read_nar(rel, h, v2)
        if not complete:
            log(f"PUT {rel} {h} aborted")
            self.close_connection = True
            return
        with LOCK:
            state = load()
            result = self.finish_nar(state, upload_id, data, v2)
            save(state)
        if control(f"drop-{h}"):
            os.remove(f"{CONTROL}/drop-{h}")
            log(f"PUT {rel} {h} dropped")
            self.close_connection = True
            return
        log(f"PUT {rel} {h} {result_name(result)}")
        self.v1_reply(result)

    def read_nar(self, rel, h, v2):
        global ACTIVE_NARS, PEAK_NARS
        if v2:
            with LOCK:
                ACTIVE_NARS += 1
                PEAK_NARS = max(PEAK_NARS, ACTIVE_NARS)
                with open(f"{ROOT}/peak.log", "w") as f:
                    f.write(f"{PEAK_NARS}\n")
        try:
            if control(f"stall-{h}"):
                log(f"PUT {rel} {h} stalled")
                while control(f"stall-{h}"):
                    time.sleep(0.05)
            if v2 and control("delay-nars"):
                time.sleep(0.15)
            return self.body()
        finally:
            if v2:
                with LOCK:
                    ACTIVE_NARS -= 1

    def finish_nar(self, state, upload_id, data, v2):
        upload = current_upload(state, upload_id)
        if upload is None:
            return {"error": "unknown_upload"}
        decl = upload["decl"]
        result = current_result(state, upload, decl)
        if result.get("state") == "verified" and not v2:
            return publish(state, upload_id)
        if result.get("state") != "open":
            return result
        h = basename(decl["store_path"])[:32]
        if control(f"reject-{h}") or len(data) != decl["nar_size"] or "sha256:" + nix32(hashlib.sha256(data).digest()) != decl["nar_hash"]:
            upload["state"] = "verification_failed"
            return {"error": "verification_failed"}
        if quota(state, decl):
            return {"error": "quota_exceeded"}
        upload["state"] = "verified"
        return {"state": "verified"} if v2 else publish(state, upload_id)

    def log_message(self, *args):
        pass


os.makedirs(CONTROL, exist_ok=True)
http.server.ThreadingHTTPServer(("127.0.0.1", 8486), Handler).serve_forever()
