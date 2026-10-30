# Recovers true transparency from the same render on black and on white, cuts each size out,
# and writes the PNGs plus a multi-size .ico.
import io, struct, sys
from pathlib import Path
from PIL import Image

here = Path(__file__).parent
black = Image.open(here / "r-black.png").convert("RGB")
white = Image.open(here / "r-white.png").convert("RGB")
SIZES = [1024, 512, 256, 128, 96, 64, 48, 40, 32, 24, 20, 16]
out = here / "final"; out.mkdir(exist_ok=True)

def cut(size, x):
    b = black.crop((x, 0, x + size, size)).load()
    w = white.crop((x, 0, x + size, size)).load()
    img = Image.new("RGBA", (size, size))
    px = img.load()
    for j in range(size):
        for i in range(size):
            br, bg, bb = b[i, j]; wr, wg, wb = w[i, j]
            a = 255 - max(0, min(255, round(((wr - br) + (wg - bg) + (wb - bb)) / 3)))
            if a <= 0:
                px[i, j] = (0, 0, 0, 0)
            else:
                f = 255 / a
                px[i, j] = (min(255, round(br * f)), min(255, round(bg * f)), min(255, round(bb * f)), a)
    return img

x = 0
imgs = {}
for s in SIZES:
    imgs[s] = cut(s, x)
    imgs[s].save(out / f"{s}.png")
    x += s + 8

# .ico with PNG-compressed entries (Windows Vista+), largest first is not required; keep ascending
ico_sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
blobs = []
for s in ico_sizes:
    buf = io.BytesIO(); imgs[s].save(buf, "PNG"); blobs.append(buf.getvalue())
header = struct.pack("<HHH", 0, 1, len(ico_sizes))
offset = 6 + 16 * len(ico_sizes)
entries = b""
for s, data in zip(ico_sizes, blobs):
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
(out / "icon.ico").write_bytes(header + entries + b"".join(blobs))
print("ok", sorted(p.name for p in out.iterdir()))
