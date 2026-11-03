"""Serve the real TVFS web UI (files_web) with a mock API, for any browser.

    TVFS_WEB_ROOT=<unpacked TVFS-FULL>/files_web python serve.py [port]

Open http://127.0.0.1:8765/upload.html (logged in: the page reads the
`tvfs_tier` cookie, set it with `document.cookie = "tvfs_tier=2;path=/"`).
`states.js` drives the page into the stage-2 states (file list, progress,
results, stats card, unfinished uploads) so they can be measured with dump.js.
Uploads to the mock succeed slowly (chunks wait 1.5 s) so progress stays visible.
"""
import http.server
import json
import os
import socketserver
import sys
import time
import urllib.parse

ROOT = os.environ.get("TVFS_WEB_ROOT", ".")
NOW = int(time.time() * 1000)
FILES = [
    {"id": "1", "name": "sample-video.mkv", "link": "https://files.tomasekvalla.cz/files/videos/sample-video.mkv", "category": "video",
     "size": 1503238553, "uploadedAt": NOW - 3600000, "expiresAt": NOW + 6 * 86400000, "thumbnail": None},
    {"id": "2", "name": "sample-track.flac", "link": "https://files.tomasekvalla.cz/files/audio/sample-track.flac", "category": "audio",
     "size": 40050000, "uploadedAt": NOW - 7200000, "expiresAt": NOW + 2 * 86400000, "thumbnail": None},
    {"id": "3", "name": "notes.md", "link": "https://files.tomasekvalla.cz/files/text/notes.md", "category": "file",
     "size": 2100, "uploadedAt": NOW - 9000000, "expiresAt": NOW + 86400000, "thumbnail": None},
    {"id": "4", "name": "wallpaper.png", "link": "https://files.tomasekvalla.cz/files/pictures/wallpaper.png", "category": "image",
     "size": 900000, "uploadedAt": NOW - 99000000, "expiresAt": NOW - 1000, "thumbnail": None},
]
PENDING = [{"uploadId": "remote_1", "filename": "holiday-4k.mov", "fileSize": 3000000000, "totalChunks": 100, "receivedChunks": 42,
            "createdAt": NOW - 7200000, "expiresAt": NOW + 22 * 3600000}]


class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=ROOT, **k)

    def log_message(self, *a):
        pass

    def _json(self, obj, code=200):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        p = urllib.parse.urlparse(self.path)
        if p.path == "/api/status":
            return self._json({"backend": "UP", "usedBytes": 13314398618, "usedGB": "12.40 GB", "totalGB": 32, "tier": 2,
                               "userVersion": 2, "pendingUploads": PENDING})
        if p.path == "/api/user/sync":
            return self._json({"mode": "full", "version": 2, "files": FILES})
        if p.path.startswith("/api/upload/status/"):
            return self._json({"active": False})
        if p.path.startswith("/api/upload/styled-link/"):
            return self._json({"ready": True, "styledLink": "[file | 1.00 MB | TomasekValla Filestream System](https://x)", "thumbnail": None})
        if p.path.startswith("/api/"):
            return self._json({})
        if p.path in ("/", "/upload"):
            self.path = "/upload.html"
        return super().do_GET()

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0") or 0)
        self.rfile.read(n)
        p = urllib.parse.urlparse(self.path).path
        if p == "/api/upload/init":
            return self._json({"uploadId": "mock_1", "recommendedParallel": 4})
        if p == "/api/upload/chunk":
            time.sleep(1.5)
            return self._json({"success": True})
        if p in ("/api/upload", "/api/upload/complete"):
            time.sleep(1.5)
            return self._json({"link": "https://files.tomasekvalla.cz/files/download/mock_file.bin", "type": "file", "filename": "mock_file.bin",
                               "size": 1000, "fileId": "f_mock", "styledLinkPending": True})
        if p == "/api/upload/batch/init":
            return self._json({"batchId": "batch_mock"})
        if p == "/api/upload/batch/complete":
            return self._json({"batchId": "batch_mock", "link": "https://files.tomasekvalla.cz/files/batch/batch_mock.html",
                               "styledLink": "[2 Files (2.00 KB) - TomasekValla Filestream System](https://x)", "files": []})
        return self._json({"valid": True, "tier": 2})

    def do_DELETE(self):
        return self._json({"deleted": True})


socketserver.ThreadingTCPServer.allow_reuse_address = True
port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
with socketserver.ThreadingTCPServer(("127.0.0.1", port), H) as srv:
    print(f"serving {ROOT} on http://127.0.0.1:{port}/upload.html", flush=True)
    srv.serve_forever()
