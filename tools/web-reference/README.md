# Rendering the real web UI as the visual reference

The native UI must look **1:1** like the web TVFS (`files_web/upload.html`). Never guess the layout: render the page and measure it.

```
sudo apt-get install -y python3-pyqt5 python3-pyqt5.qtwebengine xvfb imagemagick
Xvfb :99 -screen 0 1400x1000x24 &  export DISPLAY=:99
export TVFS_WEB_ROOT=<unpacked TVFS-FULL>/files_web
python3 render.py out.png 1280 2000 /upload.html auth     # 'auth' = logged in (sets the tvfs_tier cookie); omit for logged out
DUMPJS=dump.js python3 render.py dump.json 1280 1000 /upload.html auth   # computed layout/styles of every element
DUMPJS=cards.js python3 render.py cards.json 1280 1000 /upload.html auth # exact texts of the settings cards
```
`render.py` serves `files_web` with a tiny mock API (`/api/status`, `/api/user/sync`, ...) and renders it in QtWebEngine (this is only a *reference*; the app itself never uses a webview).
Reference screenshots of the web UI are kept outside the repository.

Native screenshots (headless): `SLINT_BACKEND=winit-software NATIVEFS_DEMO=1 target/debug/naTiVeFS` (software renderer) or
`LIBGL_ALWAYS_SOFTWARE=1 ...` (the real OpenGL/femtovg renderer via Mesa), then `import -window root shot.png`.
`NATIVEFS_DEMO=0` = logged-out screen with fake status chips, `NATIVEFS_DEMO=1` = logged-in with fake recent files (debug builds only).

## Theme measurements (added in V5)
`render_prejs_dump.py` = `render.py` that runs a JS snippet (6th argument) BEFORE the dump, `render_prejs_shot.py` the same for screenshots.
Light mode is one click on the web's theme button:
```
NOTRANS="var st=document.createElement('style');st.textContent='*{transition:none!important;animation:none!important}';document.head.appendChild(st);"
DUMPJS=dump.js python3 render_prejs_dump.py dark.json  1280 1000 /upload.html auth "$NOTRANS"
DUMPJS=dump.js python3 render_prejs_dump.py light.json 1280 1000 /upload.html auth "$NOTRANS document.getElementById('themeToggle').click();"
python3 theme_diff.py dark.json light.json
```
Gotcha: QtWebEngine persists the web's `localStorage` between runs, so after one light run the next "default" run also starts light. Delete the profile dir or click the toggle again.
Results of that run are kept outside the repository.
