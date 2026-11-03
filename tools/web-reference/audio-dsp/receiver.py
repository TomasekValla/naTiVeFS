# Usage: python -I receiver.py <folder>   (127.0.0.1:5199, local only)
# Saves POST bodies to <dir>/<path> (local only, for dumping measurements out of the browser preview).
import http.server, os, sys
OUT = sys.argv[1]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        name = os.path.basename(self.path) or 'dump.json'
        open(os.path.join(OUT, name), 'wb').write(self.rfile.read(n))
        self.send_response(204); self.send_header('Access-Control-Allow-Origin', '*'); self.end_headers()
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', 5199), H).serve_forever()
