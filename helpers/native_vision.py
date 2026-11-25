"""Local OCR (Tesseract) and vectorized color-cluster locator for native screenshots."""
import json
import os
import subprocess
import sys


def run_ocr(png_path):
    from PIL import Image, ImageOps

    with Image.open(png_path) as im:
        width, height = im.size
        gray = im.convert("L")

    def tsv_on_image(img, psm, ox=0, oy=0, scale=1):
        import tempfile
        with tempfile.NamedTemporaryFile(suffix=".png") as tmp:
            img.save(tmp.name)
            proc = subprocess.run(
                ["/usr/bin/tesseract", tmp.name, "stdout", "--psm", str(psm), "tsv"],
                capture_output=True,
                text=True,
                timeout=15,
            )
        if proc.returncode != 0:
            return []
        out = []
        for line in proc.stdout.splitlines()[1:]:
            cols = line.split("\t")
            if len(cols) < 12:
                continue
            text = cols[11].strip()
            if not text:
                continue
            try:
                conf = float(cols[10])
                x = int(cols[6]) // scale + ox
                y = int(cols[7]) // scale + oy
                w = max(1, int(cols[8]) // scale)
                h = max(1, int(cols[9]) // scale)
            except ValueError:
                continue
            if conf < 10:
                continue
            out.append({
                "text": text,
                "x": x,
                "y": y,
                "w": w,
                "h": h,
                "cx": x + w // 2,
                "cy": y + h // 2,
                "conf": round(conf, 1),
            })
        return out

    # Run 3x upscaled autocontrast OCR over overlapping 3x2 tiles so small UI fonts
    # aren't drowned out by large images/photos, then deduplicate tile-border overlaps.
    raw_items = []
    cols_n, rows_n = 2, 3
    pad = 40
    scale = 3
    for r in range(rows_n):
        for c in range(cols_n):
            x0 = max(0, c * (width // cols_n) - (pad if c > 0 else 0))
            y0 = max(0, r * (height // rows_n) - (pad if r > 0 else 0))
            x1 = min(width, (c + 1) * (width // cols_n) + (pad if c + 1 < cols_n else 0))
            y1 = min(height, (r + 1) * (height // rows_n) + (pad if r + 1 < rows_n else 0))
            crop = gray.crop((x0, y0, x1, y1))
            up = ImageOps.autocontrast(
                crop.resize((crop.width * scale, crop.height * scale), Image.Resampling.LANCZOS),
                cutoff=1,
            )
            raw_items.extend(tsv_on_image(up, 6, ox=x0, oy=y0, scale=scale))

    if len(raw_items) < 3:
        up2 = ImageOps.autocontrast(
            gray.resize((width * 2, height * 2), Image.Resampling.LANCZOS),
            cutoff=1,
        )
        raw_items.extend(tsv_on_image(up2, 11, ox=0, oy=0, scale=2))

    # Deduplicate overlapping tokens (keep higher confidence).
    items = []
    for it in sorted(raw_items, key=lambda d: -d["conf"]):
        dup = False
        for kept in items:
            if (
                abs(it["cx"] - kept["cx"]) <= max(12, min(it["w"], kept["w"]) // 2)
                and abs(it["cy"] - kept["cy"]) <= max(8, min(it["h"], kept["h"]) // 2)
            ):
                dup = True
                break
        if not dup:
            items.append(it)

    items.sort(key=lambda d: (d["y"], d["x"]))

    # Group words on the same horizontal line into phrase lines for multi-word click_text.
    lines = []
    for it in items:
        placed = False
        for ln in lines:
            last = ln[-1]
            same_row = abs(it["cy"] - last["cy"]) <= max(10, max(it["h"], last["h"]) * 0.75)
            close_x = -8 <= (it["x"] - (last["x"] + last["w"])) <= max(48, last["h"] * 3)
            if same_row and close_x:
                ln.append(it)
                placed = True
                break
        if not placed:
            lines.append([it])

    line_entries = []
    for ln in lines:
        ln.sort(key=lambda d: d["x"])
        x0 = min(d["x"] for d in ln)
        y0 = min(d["y"] for d in ln)
        x1 = max(d["x"] + d["w"] for d in ln)
        y1 = max(d["y"] + d["h"] for d in ln)
        line_entries.append({
            "text": " ".join(d["text"] for d in ln),
            "x": x0,
            "y": y0,
            "w": x1 - x0,
            "h": y1 - y0,
            "cx": (x0 + x1) // 2,
            "cy": (y0 + y1) // 2,
            "conf": round(sum(d["conf"] for d in ln) / len(ln), 1),
        })

    return {
        "ok": True,
        "width": width,
        "height": height,
        "count": len(items),
        "lines": line_entries[:250],
        "items": items[:400],
        "truncated": len(items) > 400 or len(line_entries) > 250,
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
