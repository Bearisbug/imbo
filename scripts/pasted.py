#!/usr/bin/env python3
"""Test helpers for pasted images (docs/TEST.md TC-064..TC-066).

usage:
  pasted.py png <out.png> <width> <height>
      a real PNG of one colour, for pasting into Claude Code or Codex
  pasted.py transcript <out.jsonl> <numbers>:<images> [<numbers>:<images> ...]
      a Claude Code transcript with one user prompt per argument, e.g.
      `1,2:a.png,b.jpg 1:c.png` = a prompt pasting a.png as [Image #1] and
      b.jpg as [Image #2], then a later prompt pasting c.png as [Image #1]
"""
import base64
import json
import os
import struct
import sys
import zlib


def png(path, w, h):
    raw = b"".join(b"\x00" + b"\xcc\x55\x22" * w for _ in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def transcript(path, prompts):
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    lines = []
    for p in prompts:
        nums, _, files = p.partition(":")
        ids = [int(n) for n in nums.split(",")]
        files = files.split(",")
        if len(ids) != len(files):
            sys.exit(f"{p}: as many numbers as images")
        text = " ".join(f"[Image #{n}]" for n in ids)
        content = [{"type": "text", "text": text}]
        for f in files:
            kind = "image/jpeg" if f.lower().endswith((".jpg", ".jpeg")) else "image/png"
            data = base64.b64encode(open(f, "rb").read()).decode()
            content.append({"type": "image", "source": {"type": "base64", "media_type": kind, "data": data}})
        lines.append({"type": "user", "imagePasteIds": ids, "message": {"role": "user", "content": content}})
        lines.append({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}})
    with open(path, "w") as f:
        f.write("\n".join(json.dumps(l) for l in lines) + "\n")


def main():
    a = sys.argv[1:]
    if len(a) == 4 and a[0] == "png":
        png(a[1], int(a[2]), int(a[3]))
    elif len(a) >= 3 and a[0] == "transcript":
        transcript(a[1], a[2:])
    else:
        print(__doc__, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
