#!/usr/bin/env python3
"""Generate the Depth icon set.

Everything ships from this one file: the redrawn DEPTH lockup (three rounded bars plus the
`DEPTH` wordmark with its bullet), the multi-size Windows icon the installers and the
executable embed, the PNGs used by docs, and the raw coverage mask the tray icon tints by
status. The artwork is described by the constants below, so restyling the app means editing
fractions here and re-running:

    python scripts/make_icon.py

Outputs (all under assets/icon, all deterministic on one machine - re-running rewrites
identical bytes; the lockup sizes additionally need a bold sans font, see FONTS):

    depth.ico    16/20/24/32/40/48/64/96/128 px DIB entries + a 256 px PNG entry
    depth.svg    vector master for docs and future rasterisers
    png/*.png            RGBA PNGs at 32..1024 px
    tray-32.mask         raw 32x32 coverage pairs the tray icon tints (see src/tray.rs)
    preview.png          contact sheet: every size on light/dark backgrounds plus tray states

Pillow is the only requirement, and it is used purely as a rasteriser. `--all-bmp` writes the
256 px entry as a DIB too, which is the fallback if a resource compiler or installer builder
turns out to dislike PNG-compressed icon entries.
"""

from __future__ import annotations

import argparse
import struct
import sys
from io import BytesIO
from pathlib import Path

try:
    from PIL import Image, ImageChops, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - only hit on a Python without Pillow
    sys.exit("Pillow is required: python -m pip install pillow")

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = ROOT / "assets" / "icon"

# ---------------------------------------------------------------- artwork

# The badge is a white rounded square inset from the canvas edge, so the black DEPTH mark
# reads on light and dark surfaces alike instead of bleeding to the edges.
BADGE_INSET = 0.06  # fraction of the canvas
CORNER_RADIUS = 0.24  # fraction of the badge side

BADGE = (0xFF, 0xFF, 0xFF)
GLYPH = (0x00, 0x00, 0x00)

# The mark is the DEPTH lockup: three rounded bars (the middle one taller) plus the `DEPTH`
# wordmark with its bullet. Below LOCKUP_AT the wordmark would smear into a grey block, so
# small sizes show the bars alone while larger ones show the full lockup.
BARS = {"bars": 3, "width": 0.14, "gap": 0.07, "heights": (0.62, 0.80, 0.62)}
LOCKUP_AT = 64  # px: at and above this size the full bars-plus-wordmark lockup is used

# Full-lockup geometry, measured from the logo as fractions of the lockup box (which spans
# LOCKUP_WIDTH of the badge side at aspect LOCKUP_ASPECT h/w): bars first, then the wordmark.
LOCKUP_WIDTH = 0.85
LOCKUP_ASPECT = 119 / 183
LOCKUP_BARS = {"x0": 1 / 183, "width": 19 / 183, "gap": 11 / 183,
               "heights": (88 / 119, 112 / 119, 88 / 119)}
LOCKUP_TEXT_X0 = 91 / 183  # the wordmark starts here and runs to the lockup box edge
LOCKUP_TEXT_CAP = 19 / 119  # cap height, as a fraction of the lockup box height
WORDMARK = "DEPTH \u2022"
# Bold grotesque for the wordmark: Arial Bold matches the logo, Segoe UI Bold is the Windows
# fallback, DejaVu Sans Bold covers machines without either.
FONTS = ("C:/Windows/Fonts/arialbd.ttf", "C:/Windows/Fonts/segoeuib.ttf",
         "DejaVuSans-Bold", "DejaVuSans-Bold.ttf")
ARIAL_CAP = 0.716  # Arial's cap height as a fraction of its em size

ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 96, 128, 256)
PNG_SIZES = (32, 64, 128, 256, 512, 1024)
PNG_ENTRY_ABOVE = 128  # ICO entries larger than this are stored as PNG unless --all-bmp
TRAY_SIZE = 32

# Status colours the tray tints the mask with; they mirror src/tray.rs.
TRAY_COLORS = ((0x8A, 0x8A, 0x8A), (0x3B, 0xA5, 0x5C), (0xE0, 0x4F, 0x4F))

SUPERSAMPLE = 8  # draw each size this much larger, then downsample for anti-aliasing
MIN_DRAW = 512  # ...but never draw smaller than this, so 16 px still gets clean edges


def lockup(size: int) -> bool:
    """Whether an icon of `size` px shows the full lockup rather than the bars alone."""
    return size >= LOCKUP_AT


def draw_bars(
    draw: ImageDraw.ImageDraw,
    left: float,
    centre_y: float,
    bar_width: float,
    gap: float,
    heights: tuple[float, ...],
) -> None:
    """Three rounded bars from `left`, each `heights` tall and centred on `centre_y`."""
    x = left
    for height in heights:
        draw.rounded_rectangle(
            (x, centre_y - height / 2, x + bar_width, centre_y + height / 2),
            radius=bar_width / 2,
            fill=255,
        )
        x += bar_width + gap


def font_path() -> str:
    """The first usable bold font for the wordmark, or an error when none is installed."""
    for name in FONTS:
        try:
            ImageFont.truetype(name, 16)
            return name
        except OSError:
            continue
    sys.exit(
        "No bold font found (tried %s); install Arial Bold or Segoe UI Bold "
        "to render the wordmark." % ", ".join(FONTS)
    )


def fit_font(draw: ImageDraw.ImageDraw, box_width: float, cap_px: float) -> ImageFont.FreeTypeFont:
    """The wordmark font at `cap_px` cap height, shrunk until it fits `box_width`."""
    path = font_path()
    size = max(8, round(cap_px / ARIAL_CAP))
    while size > 8:
        font = ImageFont.truetype(path, size)
        left, _, right, _ = draw.textbbox((0, 0), WORDMARK, font=font)
        if right - left <= box_width:
            return font
        size -= 1
    return ImageFont.truetype(path, 8)


def shape(size: int) -> tuple[Image.Image, Image.Image]:
    """The mark's two coverage masks at high resolution: the badge, and the glyph inside it.

    Both are drawn `SUPERSAMPLE` times larger than the icon and downsampled later, so the
    rounded corners, the bar caps and the wordmark stay clean at 16 px. Everything the app
    draws comes from here.
    """
    hi = max(MIN_DRAW, size * SUPERSAMPLE)
    inset = BADGE_INSET * hi
    badge_side = hi - 2 * inset
    centre = hi / 2

    badge = Image.new("L", (hi, hi), 0)
    ImageDraw.Draw(badge).rounded_rectangle(
        (inset, inset, hi - inset - 1, hi - inset - 1),
        radius=CORNER_RADIUS * badge_side,
        fill=255,
    )

    glyph = Image.new("L", (hi, hi), 0)
    draw = ImageDraw.Draw(glyph)
    if lockup(size):
        box_width = LOCKUP_WIDTH * badge_side
        box_height = box_width * LOCKUP_ASPECT
        box_left = inset + (badge_side - box_width) / 2
        bar_width = LOCKUP_BARS["width"] * box_width
        draw_bars(
            draw,
            box_left + LOCKUP_BARS["x0"] * box_width,
            centre,
            bar_width,
            LOCKUP_BARS["gap"] * box_width,
            tuple(height * box_height for height in LOCKUP_BARS["heights"]),
        )
        text_left = box_left + LOCKUP_TEXT_X0 * box_width
        font = fit_font(draw, box_left + box_width - text_left, LOCKUP_TEXT_CAP * box_height)
        # Centre the ink box itself, so fonts with different line gaps land identically.
        ink_left, ink_top, ink_right, ink_bottom = draw.textbbox((0, 0), WORDMARK, font=font)
        draw.text(
            (text_left - ink_left, centre - (ink_bottom - ink_top) / 2 - ink_top),
            WORDMARK,
            font=font,
            fill=255,
        )
    else:
        span = (BARS["bars"] * BARS["width"] + (BARS["bars"] - 1) * BARS["gap"]) * badge_side
        draw_bars(
            draw,
            inset + (badge_side - span) / 2,
            centre,
            BARS["width"] * badge_side,
            BARS["gap"] * badge_side,
            tuple(height * badge_side for height in BARS["heights"]),
        )
    return badge, glyph


def render(size: int) -> Image.Image:
    """Draw the icon at exactly `size` px: white badge, black DEPTH mark, transparent outside."""
    assert size >= 4, size
    badge, glyph = shape(size)

    # Compose at the final size. Colour and alpha are resized separately so the transparent
    # margin cannot bleed dark pixels into the badge edge.
    cover = badge.resize((size, size), Image.LANCZOS)
    mask = glyph.resize((size, size), Image.LANCZOS).convert("RGB")
    base = Image.new("RGB", (size, size), BADGE)
    dark = Image.new("RGB", (size, size), GLYPH)
    rgb = ImageChops.add(
        ImageChops.multiply(base, ImageChops.invert(mask)),
        ImageChops.multiply(dark, mask),
    )

    icon = rgb.convert("RGBA")
    icon.putalpha(cover)
    return icon


def tray_mask() -> bytes:
    """The 32x32 mask the tray icon tints: interleaved (badge, glyph) coverage bytes, flat.

    The badge is stored without its colour because the app paints it in the status colour;
    the glyph is stored separately so it can be blended back in as white.
    """
    badge, glyph = shape(TRAY_SIZE)
    cover = badge.resize((TRAY_SIZE, TRAY_SIZE), Image.LANCZOS)
    mask = glyph.resize((TRAY_SIZE, TRAY_SIZE), Image.LANCZOS)
    return bytes(byte for pair in zip(cover.tobytes(), mask.tobytes()) for byte in pair)


def svg() -> str:
    """The same artwork as vector shapes, for docs and for anyone re-exporting it."""
    canvas = 512.0
    inset = BADGE_INSET * canvas
    side = canvas - 2 * inset
    centre = canvas / 2

    box_width = LOCKUP_WIDTH * side
    box_height = box_width * LOCKUP_ASPECT
    box_left = inset + (side - box_width) / 2

    bar_width = LOCKUP_BARS["width"] * box_width
    gap = LOCKUP_BARS["gap"] * box_width
    bars_left = box_left + LOCKUP_BARS["x0"] * box_width
    shapes = []
    for index, height in enumerate(LOCKUP_BARS["heights"]):
        bar_height = height * box_height
        shapes.append(
            '    <rect x="%.2f" y="%.2f" width="%.2f" height="%.2f" rx="%.2f"/>'
            % (
                bars_left + index * (bar_width + gap),
                centre - bar_height / 2,
                bar_width,
                bar_height,
                bar_width / 2,
            )
        )

    def colour(rgb: tuple[int, int, int]) -> str:
        return "#%02X%02X%02X" % rgb

    # Same width fit as the raster, so the vector master matches the PNGs.
    text_x = box_left + LOCKUP_TEXT_X0 * box_width
    probe = ImageDraw.Draw(Image.new("L", (8, 8), 0))
    font_size = fit_font(probe, box_left + box_width - text_x, LOCKUP_TEXT_CAP * box_height).size

    return "\n".join(
        [
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512"',
            '     role="img" aria-label="Depth">',
            '  <rect x="%.2f" y="%.2f" width="%.2f" height="%.2f" rx="%.2f" fill="%s"/>'
            % (inset, inset, side, side, CORNER_RADIUS * side, colour(BADGE)),
            '  <g fill="%s">' % colour(GLYPH),
            *shapes,
            "  </g>",
            '  <text x="%.2f" y="%.2f" font-family="Arial, \'Segoe UI\', sans-serif"'
            % (text_x, centre),
            '        font-weight="700" font-size="%d" text-anchor="start"'
            % font_size,
            '        dominant-baseline="central" fill="%s">DEPTH &#8226;</text>' % colour(GLYPH),
            "</svg>",
            "",
        ]
    )


# ---------------------------------------------------------------- file formats


def dib_bytes(icon: Image.Image) -> bytes:
    """One icon image as a BITMAPINFOHEADER DIB, the way rc.exe stores an RT_ICON.

    Height is doubled because the format describes both the colour bitmap and the AND mask
    below it. The mask is all zeroes: the alpha channel is authoritative for 32-bpp icons.
    """
    width, height = icon.size
    pixels = icon.convert("RGBA").load()
    colour = bytearray()
    for y in range(height - 1, -1, -1):  # DIB rows run bottom-up
        for x in range(width):
            r, g, b, a = pixels[x, y]
            colour += bytes((b, g, r, a))
    stride = ((width + 31) // 32) * 4
    mask = bytes(stride * height)
    header = struct.pack(
        "<IiiHHIIiiII",
        40,  # biSize
        width,
        height * 2,  # biHeight: colour + mask
        1,  # biPlanes
        32,  # biBitCount
        0,  # biCompression: BI_RGB
        len(colour) + len(mask),  # biSizeImage
        0,
        0,
        0,
        0,
    )
    return header + bytes(colour) + mask


def png_bytes(icon: Image.Image) -> bytes:
    buffer = BytesIO()
    icon.save(buffer, "PNG", optimize=True)
    return buffer.getvalue()


def write_ico(path: Path, icons: list[Image.Image], png_entries: bool) -> None:
    """Assemble a multi-size .ico by hand: Pillow cannot mix DIB and PNG entries in one file."""
    entries, blobs = [], []
    offset = 6 + 16 * len(icons)
    for icon in icons:
        size = icon.size[0]
        blob = png_bytes(icon) if png_entries and size > PNG_ENTRY_ABOVE else dib_bytes(icon)
        entries.append(
            struct.pack(
                "<BBBBHHII",
                size if size < 256 else 0,
                size if size < 256 else 0,
                0,  # bColorCount: 0 for 32-bpp
                0,  # bReserved
                1,  # wPlanes
                32,  # wBitCount
                len(blob),
                offset,
            )
        )
        blobs.append(blob)
        offset += len(blob)
    path.write_bytes(struct.pack("<HHH", 0, 1, len(icons)) + b"".join(entries) + b"".join(blobs))


def tint(mask: bytes, colour: tuple[int, int, int]) -> Image.Image:
    """Compose the tray mask the way src/tray.rs does, for the preview sheet."""
    icon = Image.new("RGBA", (TRAY_SIZE, TRAY_SIZE))
    icon.putdata(
        [
            (
                round(colour[0] * (255 - g) / 255 + 255 * g / 255),
                round(colour[1] * (255 - g) / 255 + 255 * g / 255),
                round(colour[2] * (255 - g) / 255 + 255 * g / 255),
                b,
            )
            for b, g in zip(mask[0::2], mask[1::2])
        ]
    )
    return icon


def preview(icons: list[Image.Image], mask: bytes) -> Image.Image:
    """Contact sheet: every ICO size on light and dark backgrounds, plus the tray states."""
    zoom = 4
    pad = 14
    big = icons[-1]  # 256 px, shown at 1:1
    tiles = [icon.resize((icon.width * zoom,) * 2, Image.NEAREST) for icon in icons[:-1]]
    tray = [
        tint(mask, colour).resize((TRAY_SIZE * zoom, TRAY_SIZE * zoom), Image.NEAREST)
        for colour in TRAY_COLORS
    ]

    row_height = max(tile.height for tile in tiles) + pad
    width = max(
        sum(tile.width + pad for tile in tiles) + pad,
        big.width + pad * 2,
        sum(tile.width + pad for tile in tray) + pad,
    )
    height = pad + row_height + row_height + big.height + TRAY_SIZE * zoom + pad * 2
    sheet = Image.new("RGB", (width, height), (0xF3, 0xF3, 0xF3))

    def paste_row(row: list[Image.Image], top: int) -> None:
        x = pad
        for tile in row:
            sheet.paste(tile, (x, top), tile)
            x += tile.width + pad

    sheet.paste(Image.new("RGB", (width, row_height), (0x1E, 0x1E, 0x1E)), (0, row_height))
    paste_row(tiles, pad)
    paste_row(tiles, row_height + pad)
    sheet.paste(big, (pad, row_height * 2 + pad), big)
    paste_row(tray, row_height * 2 + pad + big.height)
    return sheet


# ---------------------------------------------------------------- entry point


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate the Depth icon set.")
    parser.add_argument(
        "--out", type=Path, default=DEFAULT_OUT, help=f"output folder (default: {DEFAULT_OUT})"
    )
    parser.add_argument(
        "--all-bmp",
        action="store_true",
        help="store the 256 px ICO entry as a DIB instead of PNG",
    )
    args = parser.parse_args()

    out: Path = args.out
    (out / "png").mkdir(parents=True, exist_ok=True)

    icons = [render(size) for size in ICO_SIZES]
    write_ico(out / "depth.ico", icons, png_entries=not args.all_bmp)
    (out / "depth.svg").write_text(svg(), encoding="utf-8")

    for size in PNG_SIZES:
        icon = icons[ICO_SIZES.index(size)] if size in ICO_SIZES else render(size)
        icon.save(out / "png" / f"depth-{size}.png", "PNG", optimize=True)

    mask = tray_mask()
    (out / "tray-32.mask").write_bytes(mask)
    preview(icons, mask).save(out / "preview.png", "PNG", optimize=True)

    written = [
        out / "depth.ico",
        out / "depth.svg",
        out / "tray-32.mask",
        out / "preview.png",
    ]
    written += sorted((out / "png").glob("*.png"))
    for path in written:
        print(f"{path.relative_to(ROOT)}  {path.stat().st_size:,} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
