#!/usr/bin/env python3
"""Download the reader fonts (the web reader's READER_FONTS list in files_backend/upload.js) as
static TTF files into assets/fonts/reader/, with their licence files.

Google Fonts serves static TTF instances to clients without a browser user agent; the licence
comes from github.com/google/fonts. OpenDyslexic comes from github.com/antijingoist/opendyslexic.
Run once after changing the list; the result is committed (the fonts are compiled into the app).
"""
import io
import os
import re
import sys
import urllib.request
import zipfile

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "fonts", "reader")

# (family, google/fonts licence dir, styles). Inter and JetBrains Mono Regular / SemiBold and
# Space Grotesk Bold already ship in assets/fonts for the main window; only the missing styles are fetched.
GOOGLE = [
    ("Inter", "ofl/inter", ["1,400"]),
    ("Manrope", "ofl/manrope", ["0,400", "0,700"]),
    ("Sora", "ofl/sora", ["0,400", "0,700"]),
    ("Space Grotesk", "ofl/spacegrotesk", ["0,400"]),
    ("Outfit", "ofl/outfit", ["0,400", "0,700"]),
    ("Plus Jakarta Sans", "ofl/plusjakartasans", ["0,400", "0,700", "1,400"]),
    ("Poppins", "ofl/poppins", ["0,400", "0,700", "1,400"]),
    ("Lora", "ofl/lora", ["0,400", "0,700", "1,400"]),
    ("Merriweather", "ofl/merriweather", ["0,400", "0,700", "1,400"]),
    ("PT Serif", "ofl/ptserif", ["0,400", "0,700", "1,400"]),
    ("EB Garamond", "ofl/ebgaramond", ["0,400", "0,700", "1,400"]),
    ("Atkinson Hyperlegible", "ofl/atkinsonhyperlegible", ["0,400", "0,700", "1,400"]),
    ("Lexend", "ofl/lexend", ["0,400", "0,700"]),
    ("JetBrains Mono", "ofl/jetbrainsmono", ["0,700", "1,400"]),
    ("Space Mono", "ofl/spacemono", ["0,400", "0,700", "1,400"]),
    ("Quicksand", "ofl/quicksand", ["0,400", "0,700"]),
    # one Japanese font (owner, 2026-10-25): readable, and the fallback for kana / kanji
    ("Noto Sans JP", "ofl/notosansjp", ["0,400"]),
]

OPENDYSLEXIC_ZIP = "https://github.com/antijingoist/opendyslexic/releases/download/v0.91.12/opendyslexic-0.910.12-rc2-2019.10.17.zip"


def get(url, ua=""):
    req = urllib.request.Request(url, headers={"User-Agent": ua})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def style_name(ital, wght):
    names = {400: "Regular", 700: "Bold"}
    n = names[int(wght)]
    if ital == "1":
        n = "Italic" if n == "Regular" else n + "Italic"
    return n


def main():
    os.makedirs(ROOT, exist_ok=True)
    for family, lic, styles in GOOGLE:
        axis = "ital,wght@" + ";".join(sorted(styles))
        css = get("https://fonts.googleapis.com/css2?family=" + family.replace(" ", "+") + ":" + axis).decode()
        faces = re.findall(r"font-style: (\w+);\s*font-weight: (\d+);\s*(?:font-stretch: \w+;\s*)?src: url\((\S+?\.ttf)\)", css)
        if len(faces) != len(styles):
            sys.exit(f"{family}: expected {len(styles)} styles, got {len(faces)}")
        base = family.replace(" ", "")
        for style, wght, url in faces:
            out = os.path.join(ROOT, f"{base}-{style_name('1' if style == 'italic' else '0', wght)}.ttf")
            data = get(url)
            open(out, "wb").write(data)
            print(f"{os.path.basename(out)}  {len(data) // 1024} KB")
        lic_text = get(f"https://raw.githubusercontent.com/google/fonts/main/{lic}/OFL.txt")
        open(os.path.join(ROOT, f"OFL-{base}.txt"), "wb").write(lic_text)
    z = zipfile.ZipFile(io.BytesIO(get(OPENDYSLEXIC_ZIP)))
    for name in z.namelist():
        low = name.lower()
        if low.endswith(("opendyslexic-regular.otf", "opendyslexic-bold.otf", "opendyslexic-italic.otf")):
            out = os.path.join(ROOT, os.path.basename(name))
            open(out, "wb").write(z.read(name))
            print(os.path.basename(out))
    # the release zip has no licence file; the repository has it
    open(os.path.join(ROOT, "OFL-OpenDyslexic.txt"), "wb").write(get("https://raw.githubusercontent.com/antijingoist/opendyslexic/master/OFL.txt"))


if __name__ == "__main__":
    main()
