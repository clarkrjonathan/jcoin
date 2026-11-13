"""Find and call the desktop app's local API (app 2.4.0 and later).

The app writes `api_port` and `api_token` to its data folder at start. The
data folder is, in order: the one given, $JCOIN_APP_DATA, or the default
(~/.local/share/jeffycoin, with /regtest for the test network).
Used by tools/app-api and tools/node-status.
"""
import json, os, urllib.error, urllib.request


def data_dir(given=None, regtest=False):
    if given:
        return given
    if os.environ.get("JCOIN_APP_DATA"):
        return os.environ["JCOIN_APP_DATA"]
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    path = os.path.join(base, "jeffycoin")
    return os.path.join(path, "regtest") if regtest else path


class Api:
    def __init__(self, folder):
        self.folder = folder
        with open(os.path.join(folder, "api_port")) as f:
            self.base = f"http://127.0.0.1:{f.read().strip()}"
        with open(os.path.join(folder, "api_token")) as f:
            self.token = f.read().strip()

    def call(self, method, path, body=None, timeout=15):
        """(status, parsed JSON or text). Raises OSError if nothing answers."""
        if not path.startswith("/"):
            path = "/" + path
        if not path.startswith(("/api/", "/openapi.json", "/docs")):
            path = "/api" + path
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.base + path, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.token}")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                status, raw, kind = r.status, r.read(), r.headers.get("Content-Type", "")
        except urllib.error.HTTPError as e:
            status, raw, kind = e.code, e.read(), e.headers.get("Content-Type", "")
        text = raw.decode(errors="replace")
        return status, (json.loads(text) if "json" in kind and text else text)

    def get(self, path):
        """The JSON body of a GET, or None on any failure."""
        try:
            status, body = self.call("GET", path, timeout=5)
        except OSError:
            return None
        return body if status == 200 else None


def find(given=None, regtest=False):
    """The app's API, or None if its files are missing (no app, or older than 2.4.0)."""
    try:
        return Api(data_dir(given, regtest))
    except OSError:
        return None
