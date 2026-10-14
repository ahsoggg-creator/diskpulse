"""Draws the DiskPulse icon (a pulse line over a disk) into crates/app/assets/.

Run: python scripts/make_icon.py
Outputs icon.ico (16..256 px, exe resource), icon.png (256 px), icon64.rgba (raw window icon) and
tray64.rgba (the same without the background, used as the base of the status tray icons).
"""

from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "crates" / "app" / "assets"
S = 1024  # drawn large, downscaled with antialiasing


def pulse(d: ImageDraw.ImageDraw, color: tuple[int, int, int, int], width: int) -> None:
    pts = [(150, 560), (340, 560), (420, 360), (530, 760), (620, 250), (700, 560), (874, 560)]
    d.line(pts, fill=color, width=width, joint="curve")
    for p in (pts[0], pts[-1]):
        d.ellipse((p[0] - width // 2, p[1] - width // 2, p[0] + width // 2, p[1] + width // 2), fill=color)


def draw(background: bool) -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if background:
        d.rounded_rectangle((32, 32, S - 32, S - 32), radius=200, fill=(30, 34, 46, 255))
        d.ellipse((150, 150, S - 150, S - 150), outline=(76, 141, 255, 255), width=46)
        d.ellipse((S // 2 - 70, S // 2 - 70, S // 2 + 70, S // 2 + 70), fill=(30, 34, 46, 255))
        pulse(d, (80, 220, 140, 255), 54)
    else:
        d.ellipse((90, 90, S - 90, S - 90), outline=(76, 141, 255, 255), width=70)
        pulse(d, (240, 240, 240, 255), 78)
    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    big = draw(True)
    big.resize((256, 256), Image.LANCZOS).save(OUT / "icon.png")
    big.resize((256, 256), Image.LANCZOS).save(
        OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    )
    (OUT / "icon64.rgba").write_bytes(big.resize((64, 64), Image.LANCZOS).tobytes())
    (OUT / "tray64.rgba").write_bytes(draw(False).resize((64, 64), Image.LANCZOS).tobytes())


if __name__ == "__main__":
    main()
