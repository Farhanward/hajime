"""Pixel-art primitives shared by every scene the brand generator draws.

The rules this module enforces, because breaking any one of them is what makes
pixel art look like a photograph of pixel art:

  Integer scale only.   Everything is composed on a small canvas and blown up by
                        a whole number with nearest-neighbour sampling. A 1.5x
                        resize invents pixels of its own and the whole picture
                        goes soft.

  No antialiasing,      Shape edges are hard, and gradients are dithered rather
  on the shapes.        than blurred: a Bayer matrix gives a band of two colours
                        the shape of a gradient while every pixel stays one of
                        the two. Type is the exception, and only below display
                        size -- see `text`. A caption nobody can read is not a
                        stricter picture, it is a worse one.

  One outline colour.   Shapes are separated by a black edge, the way sprite
                        work has been since the machines that could only afford
                        one. Soft shadows are a different medium's idea.

Text is rendered through ImageMagick rather than Pillow: this build of Pillow
has no Raqm, so it cannot shape Arabic -- it would draw the letters in isolated
forms and in the wrong order. ImageMagick here is built against raqm and
pangocairo, so `magick label:` shapes and orders Arabic correctly. That is the
whole reason for the subprocess.
"""

from __future__ import annotations

import functools
import subprocess
import tomllib
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
FONTS = ROOT / "fonts"
ICONS = ROOT / "icons"

# The fonts are carried in the repository rather than named and hoped for: the
# generator has to produce the same pixels on the machine that builds the
# release as on the one that wrote it, and a font resolved by name is whatever
# that machine happens to have installed.
FONT_PIXEL = FONTS / "PressStart2P-Regular.ttf"   # the wordmark, on an 8px grid
FONT_TERM = FONTS / "VT323-Regular.ttf"           # command lines, a real VT clone
FONT_AR = FONTS / "Almarai-Bold.ttf"              # Arabic, and any label that matters
FONT_AR_LIGHT = FONTS / "Almarai-Regular.ttf"

# Labels are set in Almarai, not in the terminal face.
#
# VT323 is a faithful copy of a video terminal, which means its strokes are one
# pixel wide by design. At the size a caption is read at, one pixel of a thin
# stroke is most of the letter, and reducing it to on-or-off pixels finishes the
# job -- the words were there and nobody could read them. Almarai carries both
# scripts, has weight to lose, and stays legible small. VT323 keeps the lines
# that are meant to look typed.
FONT_UI = FONTS / "Almarai-Regular.ttf"
FONT_UI_BOLD = FONTS / "Almarai-Bold.ttf"


# --- palette ---------------------------------------------------------------


@functools.lru_cache(maxsize=1)
def palette() -> dict:
    with open(ROOT / "palette.toml", "rb") as fh:
        return tomllib.load(fh)


def hex_of(ref: str) -> str:
    """'warm.screen' -> '#f5deb0'. Refs are resolved, not looked up twice."""
    section, _, name = ref.partition(".")
    try:
        return palette()[section][name]
    except KeyError as exc:  # a typo in a colour name should not draw silently
        raise KeyError(f"no colour {ref!r} in palette.toml") from exc


def C(ref: str, alpha: int = 255) -> tuple[int, int, int, int]:
    """A palette reference as RGBA. Accepts a literal #rrggbb too."""
    h = (ref if ref.startswith("#") else hex_of(ref)).lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    if len(h) != 6:
        raise ValueError(f"not a colour: {ref!r}")
    return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16), alpha)


def mix(a: str, b: str, t: float) -> tuple[int, int, int, int]:
    """Blend two palette colours. Used to derive, never to add a new colour."""
    ca, cb = C(a), C(b)
    return tuple(round(x + (y - x) * t) for x, y in zip(ca, cb))  # type: ignore[return-value]


def neon_ramp() -> list[str]:
    """The neon run in spectrum order, as palette refs."""
    return [
        "cold.neon_cyan",
        "cold.neon_teal",
        "cold.neon_lime",
        "cold.neon_amber",
        "cold.neon_orange",
        "cold.neon_pink",
        "cold.neon_magenta",
        "cold.neon_violet",
    ]


# --- canvas ----------------------------------------------------------------


def new(w: int, h: int, fill: str | None = None) -> Image.Image:
    return Image.new("RGBA", (w, h), C(fill) if fill else (0, 0, 0, 0))


def draw_on(img: Image.Image) -> ImageDraw.ImageDraw:
    return ImageDraw.Draw(img)


def paste(dst: Image.Image, src: Image.Image, x: int, y: int) -> None:
    dst.alpha_composite(src, (int(x), int(y)))


def upscale(img: Image.Image, k: int) -> Image.Image:
    """Nearest neighbour, integer factor. The only resize this module allows."""
    return img.resize((img.width * k, img.height * k), Image.NEAREST)


# --- dithering -------------------------------------------------------------

# 4x4 ordered (Bayer) matrix. A gradient dithered through this reads as a smooth
# ramp from a metre away and as deliberate pixels from a foot away, which is
# exactly what a background wants to do.
BAYER4 = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
]


def dither_v(w: int, h: int, top: str, bottom: str, bands: int = 16) -> Image.Image:
    """A vertical two-colour ramp, ordered-dithered.

    `bands` caps how many mixing levels are used. Fewer bands means the
    transition is visibly stepped, which suits a background drawn to look like
    it came off a machine with sixteen colours.
    """
    img = new(w, h)
    px = img.load()
    ct, cb = C(top), C(bottom)
    for y in range(h):
        t = y / max(1, h - 1)
        level = t * bands
        base = int(level)
        frac = level - base
        for x in range(w):
            threshold = (BAYER4[y % 4][x % 4] + 0.5) / 16
            step = base + (1 if frac > threshold else 0)
            f = min(1.0, step / bands)
            px[x, y] = tuple(round(a + (b - a) * f) for a, b in zip(ct, cb))
    return img


def dither_fill(img: Image.Image, box, a: str, b: str, ratio: float = 0.5) -> None:
    """Checker two colours across a box at a given density. Texture, not shape."""
    x0, y0, x1, y1 = box
    px = img.load()
    ca, cb = C(a), C(b)
    for y in range(int(y0), int(y1)):
        for x in range(int(x0), int(x1)):
            threshold = (BAYER4[y % 4][x % 4] + 0.5) / 16
            px[x, y] = cb if threshold < ratio else ca


# --- shapes ----------------------------------------------------------------


def rect(img: Image.Image, box, fill: str | None = None, outline: str | None = None) -> None:
    d = draw_on(img)
    d.rectangle(box, fill=C(fill) if fill else None, outline=C(outline) if outline else None)


def bevel(img: Image.Image, box, light: str, dark: str, w: int = 1, inset: bool = False) -> None:
    """The two-tone edge that makes a flat rectangle read as a raised key.

    Light on top and left, dark on bottom and right; `inset` swaps them, which
    is what a pressed button and a text field both are.
    """
    x0, y0, x1, y1 = (int(v) for v in box)
    hi, lo = (dark, light) if inset else (light, dark)
    d = draw_on(img)
    for i in range(w):
        d.line([(x0 + i, y0 + i), (x1 - i, y0 + i)], fill=C(hi))
        d.line([(x0 + i, y0 + i), (x0 + i, y1 - i)], fill=C(hi))
        d.line([(x0 + i, y1 - i), (x1 - i, y1 - i)], fill=C(lo))
        d.line([(x1 - i, y0 + i), (x1 - i, y1 - i)], fill=C(lo))


def panel(
    img: Image.Image,
    box,
    fill: str,
    light: str,
    dark: str,
    outline: str = "warm.outline",
    bevel_w: int = 1,
    inset: bool = False,
) -> None:
    """A raised surface: black edge, bevel inside it, fill inside that.

    Every window, button, panel and chip in the desktop is this shape at a
    different size, which is what holds the interface together.
    """
    rect(img, box, fill=fill, outline=outline)
    x0, y0, x1, y1 = (int(v) for v in box)
    bevel(img, (x0 + 1, y0 + 1, x1 - 1, y1 - 1), light, dark, w=bevel_w, inset=inset)


def hline(img: Image.Image, y: int, x0: int, x1: int, color: str) -> None:
    draw_on(img).line([(x0, y), (x1, y)], fill=C(color))


def vline(img: Image.Image, x: int, y0: int, y1: int, color: str) -> None:
    draw_on(img).line([(x, y0), (x, y1)], fill=C(color))


def wood(img: Image.Image, box, base: str = "warm.bezel", light: str = "warm.bezel_light",
         dark: str = "warm.bezel_dark", seed: int = 7, grain: int = 5) -> None:
    """A wooden surface: flat fill, then grain lines at irregular spacing.

    The irregularity is a fixed pseudo-random sequence rather than a call to
    random(): the generator must produce identical bytes on every run, or the
    committed assets churn on every regeneration.
    """
    x0, y0, x1, y1 = (int(v) for v in box)
    rect(img, box, fill=base)
    state = seed
    y = y0 + 1
    while y < y1:
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        step = grain + (state >> 16) % max(2, grain)
        tone = dark if (state >> 8) % 3 else light
        run = (state >> 4) % (x1 - x0) if x1 > x0 else 0
        hline(img, y, x0 + run // 3, x1 - run // 4, tone)
        y += step


def brass(img: Image.Image, box, seed: int = 3) -> None:
    """The control-strip material: warm metal with a dithered sheen."""
    x0, y0, x1, y1 = (int(v) for v in box)
    grad = dither_v(x1 - x0 + 1, y1 - y0 + 1, "warm.bezel_light", "warm.bezel_dark", bands=6)
    paste(img, grad, x0, y0)
    rect(img, box, outline="warm.outline")
    bevel(img, (x0 + 1, y0 + 1, x1 - 1, y1 - 1), "warm.bezel_light", "warm.bezel_dark")


def screw(img: Image.Image, x: int, y: int) -> None:
    """A three-pixel fastener. Two of these turn a rectangle into a fitted panel."""
    d = draw_on(img)
    d.rectangle((x, y, x + 2, y + 2), fill=C("warm.bezel_dark"))
    d.point((x + 1, y + 1), fill=C("warm.bezel_light"))


# --- effects ---------------------------------------------------------------


def outline_of(img: Image.Image, color: str = "mascot.outline", r: int = 1) -> Image.Image:
    """Grow the alpha by r pixels and paint it flat. Runs behind the sprite."""
    alpha = img.getchannel("A")
    for _ in range(r):
        alpha = alpha.filter(ImageFilter.MaxFilter(3))
    alpha = alpha.point(lambda v: 255 if v > 96 else 0)
    out = Image.new("RGBA", img.size, C(color))
    out.putalpha(alpha)
    return out


def with_outline(img: Image.Image, color: str = "mascot.outline", r: int = 1) -> Image.Image:
    pad = r + 1
    canvas = Image.new("RGBA", (img.width + pad * 2, img.height + pad * 2), (0, 0, 0, 0))
    canvas.alpha_composite(outline_of(img, color, r), (pad, pad))
    canvas.alpha_composite(img, (pad, pad))
    return canvas


def drop_shadow(img: Image.Image, dx: int = 2, dy: int = 2, color: str = "#000000",
                alpha: int = 110) -> Image.Image:
    sh = Image.new("RGBA", img.size, C(color, alpha))
    sh.putalpha(img.getchannel("A").point(lambda v: alpha if v > 96 else 0))
    out = Image.new("RGBA", (img.width + abs(dx), img.height + abs(dy)), (0, 0, 0, 0))
    out.alpha_composite(sh, (max(dx, 0), max(dy, 0)))
    out.alpha_composite(img, (max(-dx, 0), max(-dy, 0)))
    return out


def tint_ramp(img: Image.Image, refs: list[str], horizontal: bool = True) -> Image.Image:
    """Recolour a mask through a colour run: how the wordmark gets its spectrum.

    The source's alpha is kept and its colour discarded, so the ramp lands on the
    glyph shapes exactly.
    """
    w, h = img.size
    ramp = Image.new("RGBA", (w, h))
    px = ramp.load()
    colors = [C(r) for r in refs]
    span = max(1, (w if horizontal else h) - 1)
    for y in range(h):
        for x in range(w):
            t = (x if horizontal else y) / span
            pos = t * (len(colors) - 1)
            i = min(len(colors) - 2, int(pos))
            f = pos - i
            px[x, y] = tuple(round(a + (b - a) * f) for a, b in zip(colors[i], colors[i + 1]))
    ramp.putalpha(img.getchannel("A"))
    return ramp


def glow(img: Image.Image, radius: int = 6, strength: float = 0.55) -> Image.Image:
    """Phosphor bloom: a blurred copy of the bright parts, added underneath.

    This is the one soft thing allowed, and only because it is what a real tube
    does to a bright pixel. It is applied after upscaling, at screen resolution,
    so it never softens a pixel edge -- it surrounds it.
    """
    base = img.convert("RGBA")
    bright = base.filter(ImageFilter.GaussianBlur(radius))
    bright.putalpha(base.getchannel("A").filter(ImageFilter.GaussianBlur(radius)))
    out = Image.new("RGBA", base.size, (0, 0, 0, 0))
    out.alpha_composite(Image.blend(Image.new("RGBA", base.size, (0, 0, 0, 0)), bright, strength))
    out.alpha_composite(base)
    return out


def scanlines(img: Image.Image, period: int = 3, strength: float = 0.22) -> Image.Image:
    """Darken every `period`-th row. A CRT's line structure, not a filter preset."""
    out = img.convert("RGBA")
    overlay = Image.new("RGBA", out.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(overlay)
    dark = int(255 * strength)
    for y in range(0, out.height, period):
        d.line([(0, y), (out.width, y)], fill=(0, 0, 0, dark))
    out.alpha_composite(overlay)
    return out


def vignette(img: Image.Image, strength: float = 0.45, power: float = 2.4) -> Image.Image:
    """Corners fall off the way they do on a curved tube."""
    w, h = img.size
    mask = Image.new("L", (w, h))
    px = mask.load()
    cx, cy = w / 2, h / 2
    norm = (cx**2 + cy**2) ** 0.5
    for y in range(0, h):
        for x in range(0, w):
            d = ((x - cx) ** 2 + (y - cy) ** 2) ** 0.5 / norm
            px[x, y] = int(255 * min(1.0, (d**power) * strength))
    shade = Image.new("RGBA", (w, h), (0, 0, 0, 255))
    shade.putalpha(mask)
    out = img.convert("RGBA")
    out.alpha_composite(shade)
    return out


def crt(img: Image.Image, scan_period: int = 3, scan_strength: float = 0.20,
        bloom: int = 5, bloom_strength: float = 0.30, vig: float = 0.40) -> Image.Image:
    """The whole tube, in the order a tube does it.

    Bloom first: the phosphor spreads the light before the shadow mask cuts it
    into lines. Doing it the other way round blurs the scanlines into a grey
    wash, which is the tell of every CRT filter that looks wrong.
    """
    out = glow(img, radius=bloom, strength=bloom_strength)
    out = scanlines(out, period=scan_period, strength=scan_strength)
    if vig:
        out = vignette(out, strength=vig)
    return out


# --- text ------------------------------------------------------------------


@functools.lru_cache(maxsize=512)
def _magick_text(text: str, font: str, size: int, fill: str, interline: int) -> bytes:
    cmd = [
        "magick",
        "-background", "none",
        "-fill", fill,
        # Forward slashes, always. Handed a Windows path with backslashes,
        # ImageMagick does not fail: it quietly decides the argument is a font
        # *name*, finds no such name, and renders in its default face. Every
        # asset came out in the wrong font and nothing said so.
        "-font", font.replace("\\", "/"),
        "-pointsize", str(size),
        "-interline-spacing", str(interline),
        f"label:{text}",
        "png:-",
    ]
    res = subprocess.run(cmd, capture_output=True, check=False)
    if res.returncode != 0:
        raise RuntimeError(f"magick failed for {text!r}: {res.stderr.decode(errors='replace')}")
    return res.stdout


@functools.lru_cache(maxsize=1)
def check_fonts() -> None:
    """Prove the font files are being used before drawing anything with them.

    The failure this guards against is silent, and it is silent in the worst
    direction: the picture still renders, so the only way to notice is to know
    what Press Start 2P looks like. Press Start 2P is on an exact eight-pixel
    grid, so eight characters at size sixteen must measure 128 pixels wide. Any
    other width means the file was ignored.
    """
    for font in (FONT_PIXEL, FONT_TERM, FONT_AR, FONT_AR_LIGHT):
        if not font.exists():
            raise FileNotFoundError(f"missing font: {font}")
    probe = text("01101 OK", 16, "#ffffff", FONT_PIXEL, hard=False)
    if probe.width < 120:
        raise RuntimeError(
            f"ImageMagick ignored {FONT_PIXEL.name}: eight characters at size 16 came "
            f"out {probe.width}px wide, not ~128. The generated art would be in the "
            f"wrong font throughout."
        )


def text(
    s: str,
    size: int,
    color: str = "warm.ink",
    font: Path = FONT_PIXEL,
    hard: bool = False,
    threshold: int = 110,
    interline: int = 0,
    bold: bool = False,
) -> Image.Image:
    """Render a line of text as a bitmap.

    `hard` throws away the antialiasing: the glyph is rendered smooth, then every
    pixel is either on or off. It belongs to display type -- the wordmark, a
    heading -- where the letters are tall enough that losing their soft edge
    costs nothing.

    It is off by default, and that is a correction. Everything was hard at first,
    on the principle that a pixel picture has no grey in it. At twenty pixels a
    threshold takes a bite out of every curve and a whole stroke off some of
    them, and the result is a caption that is technically present and actually
    unreadable. Kept for the wordmark, dropped everywhere a person reads a
    sentence.
    """
    import io

    raw = _magick_text(s, str(font), size, hex_of(color) if not color.startswith("#") else color,
                       interline)
    img = Image.open(io.BytesIO(raw)).convert("RGBA")
    if bold:
        # A second impression one pixel to the right. VT323 has one weight and
        # its strokes are a single pixel by design, which is faithful to the
        # terminal it copies and thin to the point of unreadable at caption
        # size. Doubling the stroke keeps the face and the size and gives the
        # letters something to be read by.
        wide = Image.new("RGBA", (img.width + 1, img.height), (0, 0, 0, 0))
        wide.alpha_composite(img, (0, 0))
        wide.alpha_composite(img, (1, 0))
        img = wide
    if hard:
        alpha = img.getchannel("A").point(lambda v: 255 if v >= threshold else 0)
        flat = Image.new("RGBA", img.size, C(color))
        flat.putalpha(alpha)
        img = flat
    return img


def text_w(s: str, size: int, font: Path = FONT_PIXEL) -> int:
    return text(s, size, font=font).width


def draw_text(img: Image.Image, s: str, x: int, y: int, size: int, color: str = "warm.ink",
              font: Path = FONT_PIXEL, anchor: str = "lt", outline: str | None = None,
              hard: bool = False, shadow: tuple[int, int, str] | None = None,
              bold: bool = False) -> Image.Image:
    """Place text on a canvas. `anchor` takes the usual l/m/r and t/m/b pair."""
    t = text(s, size, color, font, hard=hard, bold=bold)
    if outline:
        t = with_outline(t, outline, 1)
    if shadow:
        dx, dy, sc = shadow
        t = drop_shadow(t, dx, dy, sc, 160)
    ax = {"l": 0, "m": t.width // 2, "r": t.width}[anchor[0]]
    ay = {"t": 0, "m": t.height // 2, "b": t.height}[anchor[1]]
    paste(img, t, x - ax, y - ay)
    return t


# --- icons -----------------------------------------------------------------


@functools.lru_cache(maxsize=256)
def _render_svg(path: str, size: int, stroke: str) -> bytes:
    """Rasterise one icon through librsvg at the size it will be shown.

    Rendering at the target size rather than large-then-shrink is deliberate:
    the shrink is what turns a 2px stroke into a grey smear, and grey has no
    place in a two-colour icon.
    """
    src = Path(path).read_text(encoding="utf-8").replace("currentColor", stroke)
    res = subprocess.run(
        ["magick", "-background", "none", "-density", "384", "svg:-",
         "-resize", f"{size}x{size}", "png:-"],
        input=src.encode("utf-8"), capture_output=True, check=False,
    )
    if res.returncode != 0:
        raise RuntimeError(f"magick failed on {path}: {res.stderr.decode(errors='replace')}")
    return res.stdout


def icon(name: str, size: int, color: str = "warm.ink", family: str = "lucide",
         threshold: int = 100) -> Image.Image:
    """A Lucide (or Phosphor) icon, quantised onto the pixel grid.

    Icons are not drawn by hand here. They come from an open set, are rendered
    at their final size and are then thresholded, so what lands on the desktop
    is the library's geometry expressed in whole pixels.
    """
    import io

    path = ICONS / family / f"{name}.svg"
    if not path.exists():
        raise FileNotFoundError(f"no icon {family}/{name}.svg")
    raw = _render_svg(str(path), size, hex_of(color) if not color.startswith("#") else color)
    img = Image.open(io.BytesIO(raw)).convert("RGBA")
    alpha = img.getchannel("A").point(lambda v: 255 if v >= threshold else 0)
    flat = Image.new("RGBA", img.size, C(color))
    flat.putalpha(alpha)
    return flat
