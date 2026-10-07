#!/usr/bin/env python3
"""Write installer/LICENSE.rtf from LICENSE, for the Windows installer.

The .msi is built by `dx bundle`, whose WiX template shows a licence page
only when given a licence file, and WiX reads that file as RTF. LICENSE stays
the one source of the text: release CI runs this just before bundling, so the
page always matches it. The output is committed too, so a local `dx bundle`
on Windows finds it.

Plain text in, RTF out: a monospaced font keeps the licence's indentation,
and each line becomes its own paragraph.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "LICENSE"
TARGET = ROOT / "installer" / "LICENSE.rtf"


def escape(line: str) -> str:
    out = []
    for ch in line:
        if ch in "\\{}":
            out.append("\\" + ch)
        elif ord(ch) > 127:
            # RTF \u takes a signed 16-bit value, followed by an ASCII fallback.
            code = ord(ch)
            out.append(f"\\u{code - 65536 if code > 32767 else code}?")
        else:
            out.append(ch)
    return "".join(out)


def main() -> None:
    lines = SOURCE.read_text(encoding="utf-8").splitlines()
    body = "\n".join(f"{escape(line)}\\par" for line in lines)
    rtf = "{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fmodern Consolas;}}\\f0\\fs16\n" + body + "\n}\n"
    TARGET.parent.mkdir(exist_ok=True)
    TARGET.write_text(rtf, encoding="ascii")
    print(f"wrote {TARGET.relative_to(ROOT)} ({len(lines)} lines)")


if __name__ == "__main__":
    main()
