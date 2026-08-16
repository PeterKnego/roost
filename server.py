#!/usr/bin/env python3
"""deadlight — stateless read-only project viewer.

Serves a file tree, file contents, and git working-tree status/diffs for the
project directories under ROOTS. No sessions, no websockets, no writes: every
request re-reads the filesystem, so the page can never be out of sync with the
host. Bind stays on localhost; expose it via `tailscale serve`.
"""
import json
import mimetypes
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse, parse_qs

ROOTS = [Path("/home/claude/ultima"), Path("/home/claude/projects")]
BIND = ("127.0.0.1", 8444)
MAX_FILE_BYTES = 2_000_000
STATIC_DIR = Path(__file__).resolve().parent / "static"
SKIP_DIRS = {".git", "target", "node_modules", "__pycache__", ".venv"}

TEXT_EXTENSIONS = {
    ".rs", ".toml", ".md", ".txt", ".py", ".js", ".ts", ".json", ".yaml",
    ".yml", ".sh", ".html", ".css", ".sql", ".qnt", ".tla", ".lock", ".xml",
    ".c", ".h", ".cpp", ".go", ".java", ".rb", ".proto", ".cfg", ".ini",
    ".service", ".env", ".gitignore", ".dockerignore",
}


def list_projects():
    out = []
    for root in ROOTS:
        if not root.is_dir():
            continue
        for p in sorted(root.iterdir()):
            if p.is_dir() and not p.name.startswith("."):
                out.append({"name": p.name, "path": str(p),
                            "git": (p / ".git").exists()})
    return out


def safe_resolve(raw: str) -> Path:
    p = Path(raw).resolve()
    for root in ROOTS:
        try:
            p.relative_to(root)
            return p
        except ValueError:
            continue
    raise PermissionError(f"path outside roots: {raw}")


def list_dir(path: Path):
    entries = []
    for c in sorted(path.iterdir(), key=lambda x: (x.is_file(), x.name.lower())):
        if c.name in SKIP_DIRS:
            continue
        entries.append({
            "name": c.name,
            "dir": c.is_dir(),
            "size": c.stat().st_size if c.is_file() else None,
        })
    return entries


def read_file(path: Path):
    size = path.stat().st_size
    if size > MAX_FILE_BYTES:
        return {"error": f"file too large ({size} bytes)"}
    data = path.read_bytes()
    if b"\x00" in data[:8000] and path.suffix not in TEXT_EXTENSIONS:
        return {"error": "binary file"}
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        text = data.decode("latin-1")
    return {"content": text, "size": size}


def run_git(repo: Path, *args):
    r = subprocess.run(["git", "-C", str(repo), *args],
                       capture_output=True, text=True, timeout=15)
    if r.returncode not in (0, 1):  # git diff exits 1 when differences exist
        raise RuntimeError(r.stderr.strip() or f"git exited {r.returncode}")
    return r.stdout


def git_status(repo: Path):
    branch, changes = "", []
    for line in run_git(repo, "status", "--porcelain=v2", "-b").splitlines():
        if line.startswith("# branch.head"):
            branch = line.split(" ", 2)[2]
        elif line.startswith(("1 ", "2 ")):
            parts = line.split(" ")
            path = line.split("\t")[0].split(" ", 8)[-1] if line.startswith("2 ") else parts[8]
            changes.append({"xy": parts[1], "path": path})
        elif line.startswith("? "):
            changes.append({"xy": "??", "path": line[2:]})
    return {"branch": branch, "changes": changes}


def git_diff(repo: Path, path: str | None):
    if path:
        tracked = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "--error-unmatch", path],
            capture_output=True).returncode == 0
        if not tracked:
            f = (repo / path)
            body = f.read_text(errors="replace") if f.is_file() else ""
            lines = body.splitlines()
            return (f"--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{len(lines)} @@\n"
                    + "".join(f"+{l}\n" for l in lines))
        return run_git(repo, "diff", "HEAD", "--", path)
    return run_git(repo, "diff", "HEAD")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def send_json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def send_static(self, rel: str):
        f = (STATIC_DIR / rel.lstrip("/")).resolve()
        if not f.is_file() or not str(f).startswith(str(STATIC_DIR)):
            self.send_error(404)
            return
        ctype = mimetypes.guess_type(f.name)[0] or "application/octet-stream"
        body = f.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(url.query).items()}
        try:
            if url.path == "/" or url.path == "/index.html":
                self.send_static("index.html")
            elif url.path.startswith("/static/"):
                self.send_static(url.path[len("/static/"):])
            elif url.path == "/api/projects":
                self.send_json(list_projects())
            elif url.path == "/api/tree":
                self.send_json(list_dir(safe_resolve(q["path"])))
            elif url.path == "/api/file":
                self.send_json(read_file(safe_resolve(q["path"])))
            elif url.path == "/api/git/status":
                self.send_json(git_status(safe_resolve(q["repo"])))
            elif url.path == "/api/git/diff":
                self.send_json({"diff": git_diff(safe_resolve(q["repo"]),
                                                 q.get("path"))})
            else:
                self.send_error(404)
        except PermissionError as e:
            self.send_json({"error": str(e)}, 403)
        except (FileNotFoundError, KeyError) as e:
            self.send_json({"error": f"not found: {e}"}, 404)
        except Exception as e:  # keep the viewer alive no matter what
            self.send_json({"error": str(e)}, 500)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else BIND[1]
    srv = ThreadingHTTPServer((BIND[0], port), Handler)
    print(f"deadlight listening on http://{BIND[0]}:{port}")
    srv.serve_forever()
