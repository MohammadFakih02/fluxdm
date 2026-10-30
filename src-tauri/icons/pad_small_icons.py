# Every icon size gets a small margin (1/16 of its size, like other Windows icons) and is nudged so the F, not
# the tile with its speed trails, sits in the middle. Tauri uses the largest (256 px) entry for the window,
# taskbar and tray icon, so that one matters most. Reads the untouched artwork from icon-original-256.png
# (made from icon.ico the first time), so running this again gives the same result.
import io, struct, sys
from pathlib import Path
from PIL import Image
icons = Path(sys.argv[1])
original = icons / "icon-original-256.png"
if not original.exists():
    data = (icons / "icon.ico").read_bytes()
    n = struct.unpack("<H", data[4:6])[0]
    for k in range(n):
        w, h, _, _, _, _, size, off = struct.unpack("<BBBBHHII", data[6 + 16 * k:22 + 16 * k])
        im = Image.open(io.BytesIO(data[off:off + size])).convert("RGBA")
        if im.size[0] == 256: im.save(original)
src = Image.open(original).convert("RGBA")
px = src.load(); sx = sy = c = 0
for j in range(256):
    for i in range(256):
        r, g, b, a = px[i, j]
        if a > 200 and r > 170 and g > 120: sx += i; sy += j; c += 1
fx, fy = sx / c / 255 - .5, sy / c / 255 - .5
def padded(s):
    k = 16 if s <= 64 else 4
    m = max(1, round(s / 16)); inner = (s - 2 * m) * k
    tile = src.resize((inner, inner), Image.LANCZOS)
    big = Image.new("RGBA", (s * k, s * k), (0, 0, 0, 0))
    dx = max(-m * k, min(m * k, round(-fx * inner))); dy = max(-m * k, min(m * k, round(-fy * inner)))
    big.alpha_composite(tile, (m * k + dx, m * k + dy))
    return big.resize((s, s), Image.LANCZOS)
sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
imgs = {s: padded(s) for s in sizes}
imgs[32].save(icons / "32x32.png"); imgs[128].save(icons / "128x128.png"); imgs[256].save(icons / "256x256.png")
blobs = []
for s in sizes:
    buf = io.BytesIO(); imgs[s].save(buf, "PNG"); blobs.append(buf.getvalue())
head = struct.pack("<HHH", 0, 1, len(sizes)); off = 6 + 16 * len(sizes); ent = b""
for s, b in zip(sizes, blobs):
    ent += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), off); off += len(b)
(icons / "icon.ico").write_bytes(head + ent + b"".join(blobs))
# check: where the letter ends up in the 256 px icon Windows shrinks for the tray/taskbar
p = imgs[256].load(); sx = sy = c = 0
for j in range(256):
    for i in range(256):
        r, g, b, a = p[i, j]
        if a > 200 and r > 170 and g > 120: sx += i; sy += j; c += 1
print("256 letter centre", round(sx / c, 1), round(sy / c, 1), "(127.5 = centre)")
small = imgs[256].resize((16, 16), Image.LANCZOS).resize((128, 128), Image.NEAREST)
row = Image.new("RGBA", (128 * 2 + 10, 128), (32, 32, 32, 255)); row.alpha_composite(small, (0, 0))
row.alpha_composite(imgs[256].resize((128, 128), Image.LANCZOS), (138, 0)); row.save(sys.argv[2] + "/tray-preview.png")