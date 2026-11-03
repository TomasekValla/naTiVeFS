import os, sys, json, threading, http.server, socketserver, urllib.parse
os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS", "--no-sandbox --disable-gpu --force-device-scale-factor=1")
from PyQt5.QtCore import QUrl, QTimer
from PyQt5.QtWidgets import QApplication
from PyQt5.QtWebEngineWidgets import QWebEngineView

ROOT = os.environ.get("TVFS_WEB_ROOT", "files_web")
now = 1760600000000
FILES = [
 {"id":"1","name":"sample-video.mkv","link":"https://files.tomasekvalla.cz/files/videos/sample-video.mkv","category":"videos","size":1503238553,"uploadedAt":now-3600000,"expiresAt":now+6*86400000,"thumbnail":None},
 {"id":"2","name":"sample-track.flac","link":"https://files.tomasekvalla.cz/files/audio/sample-track.flac","category":"audio","size":40050000,"uploadedAt":now-7200000,"expiresAt":now+2*86400000,"thumbnail":None},
]
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self,*a,**k): super().__init__(*a, directory=ROOT, **k)
    def log_message(self,*a): pass
    def _json(self, obj, code=200):
        b=json.dumps(obj).encode(); self.send_response(code); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        p = urllib.parse.urlparse(self.path)
        if p.path == "/api/status":
            return self._json({"backend":"UP","usedBytes":12400000000,"usedGb":"12.4 GB","totalGb":100,"tier":3,"userVersion":2,"pendingUploads":[]})
        if p.path == "/api/user/sync":
            return self._json({"mode":"full","version":2,"files":FILES})
        if p.path.startswith("/api/"): return self._json({})
        if p.path in ("/", "/upload"): self.path = "/upload.html"
        return super().do_GET()
    def do_POST(self):
        n=int(self.headers.get("Content-Length","0") or 0); self.rfile.read(n)
        return self._json({"valid":True,"tier":3})
    def end_headers(self):
        if getattr(self, "_auth", False): pass
        super().end_headers()

socketserver.TCPServer.allow_reuse_address = True
srv = socketserver.ThreadingTCPServer(("127.0.0.1", 8765), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()

def main():
    out, w, h = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    path = sys.argv[4] if len(sys.argv) > 4 else "/upload.html"
    cookie = len(sys.argv) > 5 and sys.argv[5] == "auth"
    js = sys.argv[6] if len(sys.argv) > 6 else ""
    app = QApplication(sys.argv)
    v = QWebEngineView(); v.resize(w, h)
    if cookie:
        from PyQt5.QtNetwork import QNetworkCookie
        from PyQt5.QtCore import QByteArray
        c = QNetworkCookie(QByteArray(b"tvfs_tier"), QByteArray(b"3")); c.setDomain("127.0.0.1"); c.setPath("/")
        v.page().profile().cookieStore().setCookie(c, QUrl("http://127.0.0.1:8765/"))
    v.load(QUrl("http://127.0.0.1:8765" + path)); v.show()
    def shot():
        dump = os.environ.get("DUMPJS")
        if dump:
            def done(res):
                open(out, "w").write(json.dumps(res)); app.quit()
            if js: v.page().runJavaScript(js)
            QTimer.singleShot(1500, lambda: v.page().runJavaScript(open(dump).read(), done))
            return
        if js: v.page().runJavaScript(js)
        QTimer.singleShot(900, lambda: (v.grab().save(out), app.quit()))
    QTimer.singleShot(3500, shot)
    app.exec_()
main()
