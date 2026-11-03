#!/usr/bin/env python3
"""Expand the web reader templates of files_backend/upload.js into static HTML pages, as the
server would write them, for measuring the web reader (no Node.js needed).

    python3 reader_pages.py <TVFS-FULL dir> <out dir>

Writes text.html (generateTextReaderHtml) and md.html (generateMdReaderHtml) reading sample.txt /
sample.md next to them, and copies files_web/fonts to <out>/fonts. Serve <out> with any static
server (`python3 -m http.server`); the MD page loads marked.js from jsDelivr like the real one.
"""
import json
import os
import re
import shutil
import sys

SAMPLE_MD = """# Reader sample heading
Plain paragraph with **bold**, *italic*, ~~strike~~, `inline code` and a [link](https://example.com).
Second line of the same paragraph.

## Second level heading
- bullet one
- bullet two
  - nested bullet
1. first
2. second

- [ ] open task
- [x] done task

> A quote with *emphasis*.

```rust
fn main() {
    println!("hello");
}
```

| Name | Value |
|------|-------|
| alpha | 1 |
| beta | **2** |

---
### Third level
#### Fourth level
Japanese: 日本語のテキスト、かな カナ 漢字.
"""

SAMPLE_TXT = """Plain text reader sample.
Second line, a fairly long one that should wrap when the window is narrow enough to need it, to see the wrapping.

	tab indented line
    four spaces indented
Japanese: 日本語のテキスト
"""


def template(js, fn):
    """Body of the first template literal returned by `function fn(`."""
    start = js.index(f"function {fn}(")
    a = js.index("return `", start) + len("return `")
    i = a
    while True:
        b = js.index("`", i)
        if js[b - 1] != "\\":
            return js[a:b]
        i = b + 1


def js_object_list_to_json(src):
    """READER_FONTS entries: { id: '...', label: '...', short: '...', stack: "..." | null }."""
    value = r"""('([^']*)'|"([^"]*)"|null)"""
    out = []
    for obj in re.findall(r"\{[^{}]*\}", src):
        entry = {}
        for m in re.finditer(r"(\w+)\s*:\s*" + value, obj):
            entry[m.group(1)] = None if m.group(2) == "null" else (m.group(3) if m.group(3) is not None else m.group(4))
        out.append(entry)
    return out


def main():
    root, out = sys.argv[1], sys.argv[2]
    js = open(os.path.join(root, "files_backend", "upload.js"), encoding="utf-8").read()
    fonts_src = js[js.index("const READER_FONTS = [") + len("const READER_FONTS = ") : js.index("];", js.index("const READER_FONTS = [")) + 1]
    fonts = js_object_list_to_json(fonts_src)
    chrome_css = template(js, "buildReaderChromeCss")
    chrome_html = template(js, "buildReaderChromeHtml")
    chrome_js = template(js, "buildReaderChromeJs").replace("${JSON.stringify(READER_FONTS)}", json.dumps(fonts))

    def page(fn, url, name):
        t = template(js, fn)
        html = chrome_html.replace("${escapeHtml(originalName)}", name).replace("${escapeHtml(downloadUrl)}", url).replace("${escapeHtml(downloadName)}", name)
        t = t.replace("${buildReaderChromeCss()}", chrome_css)
        t = re.sub(r"\$\{buildReaderChromeHtml\([^)]*\)\}", lambda m: html, t)
        t = re.sub(r"\$\{buildReaderChromeJs\('([^']*)'\)\}", lambda m: chrome_js.replace("${contentSelectorVarTarget}", m.group(1)), t)
        t = t.replace("${escapeHtml(originalName)}", name).replace("${FONTS_CSS_URL}", "fonts/fonts.css")
        t = re.sub(r"\$\{safeScriptJson\(\w+\)\}", json.dumps(url), t)
        t = t.replace("<\\/script>", "</script>")
        return t

    os.makedirs(out, exist_ok=True)
    open(os.path.join(out, "text.html"), "w", encoding="utf-8").write(page("generateTextReaderHtml", "sample.txt", "sample.txt"))
    open(os.path.join(out, "md.html"), "w", encoding="utf-8").write(page("generateMdReaderHtml", "sample.md", "sample.md"))
    open(os.path.join(out, "sample.md"), "w", encoding="utf-8").write(SAMPLE_MD)
    open(os.path.join(out, "sample.txt"), "w", encoding="utf-8").write(SAMPLE_TXT)
    if not os.path.isdir(os.path.join(out, "fonts")):
        shutil.copytree(os.path.join(root, "files_web", "fonts"), os.path.join(out, "fonts"))


if __name__ == "__main__":
    main()
