"""Local OCR (Tesseract) and vectorized color-cluster locator for native screenshots."""
import json
import os
import subprocess
import sys


def run_ocr(png_path):
    from PIL import Image

    with Image.open(png_path) as im:
        width, height = im.size

    def tsv_for_psm(psm):
        proc = subprocess.run(
            ["/usr/bin/tesseract", png_path, "stdout", "--psm", str(psm), "tsv"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or f"tesseract failed with {proc.returncode}")
        items = []
        lines = proc.stdout.splitlines()
        for line in lines[1:]:
            cols = line.split("\t")
            if len(cols) < 12:
                continue
            text = cols[11].strip()
            if not text:
                continue
            try:
                conf = float(cols[10])
                x = int(cols[6])
                y = int(cols[7])
                w = int(cols[8])
                h = int(cols[9])
            except ValueError:
                continue
            if w <= 0 or h <= 0 or conf < 0:
                continue
            items.append({
                "text": text,
                "x": x,
                "y": y,
                "w": w,
                "h": h,
                "cx": x + w // 2,
                "cy": y + h // 2,
                "conf": round(conf, 1),
            })
        return items

    items = tsv_for_psm(11)
    if not items:
        items = tsv_for_psm(3)
    return {
        "ok": True,
        "width": width,
        "height": height,
        "count": len(items),
        "items": items[:400],
        "truncated": len(items) > 400,
    }


def parse_color(raw):
    s = raw.strip()
    if s.startswith("#"):
        s = s[1:]
        if len(s) == 6:
            return (int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16))
        if len(s) == 3:
            return (int(s[0] * 2, 16), int(s[1] * 2, 16), int(s[2] * 2, 16))
    parts = [p.strip() for p in s.split(",")]
    if len(parts) == 3:
        r, g, b = (int(p) for p in parts)
        if all(0 <= v <= 255 for v in (r, g, b)):
            return (r, g, b)
    raise ValueError(f"Invalid color '{raw}'; expected #RRGGBB or R,G,B")


def find_color(png_path, color_str, tolerance):
    import numpy as np
    from PIL import Image

    tr, tg, tb = parse_color(color_str)
    with Image.open(png_path) as im:
        rgb = np.asarray(im.convert("RGB"), dtype=np.int16)
    h, w, _ = rgb.shape
    diff = np.abs(rgb[:, :, 0] - tr) + np.abs(rgb[:, :, 1] - tg) + np.abs(rgb[:, :, 2] - tb)
    mask = diff <= (tolerance * 3)
    ys, xs = np.where(mask)
    if len(xs) == 0:
        return {
            "ok": True,
            "color": color_str,
            "tolerance": tolerance,
            "width": w,
            "height": h,
            "total_pixels": 0,
            "clusters": [],
        }

    # Bucket pixels into 16x16 grid cells and merge adjacent cells into clusters.
    cell = 16
    buckets = {}
    for x, y in zip(xs.tolist(), ys.tolist()):
        key = (x // cell, y // cell)
        b = buckets.get(key)
        if b is None:
            buckets[key] = [1, x, y, x, y, x, y]
        else:
            b[0] += 1
            b[1] += x
            b[2] += y
            if x < b[3]:
                b[3] = x
            if y < b[4]:
                b[4] = y
            if x > b[5]:
                b[5] = x
            if y > b[6]:
                b[6] = y

    visited = set()
    clusters = []
    for seed in buckets:
        if seed in visited:
            continue
        stack = [seed]
        visited.add(seed)
        cnt = sum_x = sum_y = 0
        min_x = min_y = 10**9
        max_x = max_y = -1
        while stack:
            cx_k, cy_k = stack.pop()
            b = buckets[(cx_k, cy_k)]
            cnt += b[0]
            sum_x += b[1]
            sum_y += b[2]
            if b[3] < min_x:
                min_x = b[3]
            if b[4] < min_y:
                min_y = b[4]
            if b[5] > max_x:
                max_x = b[5]
            if b[6] > max_y:
                max_y = b[6]
            for dx in (-1, 0, 1):
                for dy in (-1, 0, 1):
                    nb = (cx_k + dx, cy_k + dy)
                    if nb in buckets and nb not in visited:
                        visited.add(nb)
                        stack.append(nb)
        clusters.append({
            "cx": sum_x // cnt,
            "cy": sum_y // cnt,
            "pixels": cnt,
            "bbox": [min_x, min_y, max_x - min_x + 1, max_y - min_y + 1],
        })

    clusters.sort(key=lambda c: c["pixels"], reverse=True)
    return {
        "ok": True,
        "color": color_str,
        "tolerance": tolerance,
        "width": w,
        "height": h,
        "total_pixels": int(len(xs)),
        "clusters": clusters[:20],
    }


def main():
    cmd = sys.argv[1]
    if cmd == "ocr":
        out = run_ocr(sys.argv[2])
    elif cmd == "find_color":
        tol = int(sys.argv[4]) if len(sys.argv) > 4 else 24
        out = find_color(sys.argv[2], sys.argv[3], tol)
    else:
        raise RuntimeError(f"Unknown vision command: {cmd}")
    print(json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()
