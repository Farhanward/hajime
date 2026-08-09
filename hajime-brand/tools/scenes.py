"""The full-screen pictures: the loader board, the splash, the wallpaper.

Each scene is drawn on a small canvas and enlarged by a whole number, which is
what makes it pixel art rather than a picture of some. The numbers below are in
that small canvas: at 480x270 and a scale of four, one unit here is four pixels
on a 1080p screen, and a one-unit line is a line you can see from across a room.

Small text is the exception and is drawn after the enlargement. A URL set on the
small canvas would be four-pixel-wide strokes -- legible, but it would take a
third of the screen. Drawing it afterwards keeps it in the same bitmap idiom at
a size a person can actually read.
"""

from __future__ import annotations

import math

from PIL import Image

import mascot
import px
from brand import brand

# 1080p, because that is the panel this machine drives. Both scenes are drawn on
# a quarter-size canvas: 480x270 * 4 = 1920x1080 exactly, with no remainder to
# hide at an edge.
SCREEN = (1920, 1080)
BASE = (480, 270)
K = 4


def lcg(seed: int):
    """A fixed pseudo-random sequence.

    Not `random`: the generator has to emit identical bytes on every run or the
    committed assets churn in every diff and the check job cries wolf.
    """
    state = seed & 0x7FFFFFFF
    while True:
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        yield state


def neon_ring(img, cx, cy, r, thickness, refs, start=0.0, sweep=360.0, alpha=255):
    """A circle whose colour runs through the neon ramp as it goes round.

    Drawn by walking angles rather than with an ellipse primitive: the colour has
    to change along the stroke, and the step is fine enough that the ring closes
    without gaps at this radius.
    """
    colors = [px.C(c, alpha) for c in refs]
    steps = max(360, int(2 * math.pi * r * 3))
    d = px.draw_on(img)
    for i in range(steps):
        t = i / steps
        ang = math.radians(start + t * sweep)
        pos = t * (len(colors) - 1)
        j = min(len(colors) - 2, int(pos))
        f = pos - j
        col = tuple(round(a + (b - a) * f) for a, b in zip(colors[j], colors[j + 1]))
        for w in range(thickness):
            x = cx + (r + w) * math.cos(ang)
            y = cy + (r + w) * math.sin(ang)
            d.point((round(x), round(y)), fill=col)


def starfield(img, box, density=140, seed=11):
    """Pixel stars, two brightnesses. Anything more is a nebula."""
    x0, y0, x1, y1 = box
    rnd = lcg(seed)
    d = px.draw_on(img)
    for _ in range(density):
        x = x0 + next(rnd) % max(1, x1 - x0)
        y = y0 + next(rnd) % max(1, y1 - y0)
        bright = next(rnd) % 5
        col = "cold.chalk" if bright == 0 else "cold.chalk_dim"
        d.point((x, y), fill=px.C(col, 190 if bright else 255))


def binary_rain(img, box, seed=5, cols=6, size=8, fade=True):
    """Columns of ones and zeros in the neon run.

    Kept inside its box on purpose. The first version drifted a few pixels for
    variety and walked straight through the text beside it, which is how
    decoration becomes noise.
    """
    x0, y0, x1, y1 = box
    rnd = lcg(seed)
    ramp = px.neon_ramp()
    step_x = max(8, (x1 - x0) // max(1, cols))
    for c in range(cols):
        x = x0 + c * step_x
        y = y0 + next(rnd) % 14
        while y < y1:
            n = 3 + next(rnd) % 4
            bits = "".join("01"[(next(rnd) >> 7) & 1] for _ in range(n))
            col = ramp[next(rnd) % len(ramp)]
            t = px.text(bits, size, col, px.FONT_PIXEL, hard=True)
            if fade:
                # Thinner towards the bottom, so the column reads as falling
                # rather than as a printed list.
                depth = (y - y0) / max(1, y1 - y0)
                t.putalpha(t.getchannel("A").point(lambda v: int(v * (1 - depth * 0.7))))
            if x + t.width <= x1:
                px.paste(img, t, x, y)
            y += size + 14 + next(rnd) % 26


def trail(img, x, y, length, height=14, seed=2, to_left=True):
    """The exhaust behind the mascot: dashes in the neon run, tapering out.

    A solid band of colour was the first attempt and it read as a flag stuck to
    the robot's feet. Broken into dashes that shorten and thin, it reads as
    motion.
    """
    ramp = px.neon_ramp()[:6]
    rnd = lcg(seed)
    rows = len(ramp)
    for i, col in enumerate(ramp):
        ry = y + round((i - rows / 2) * (height / rows))
        run = length * (1 - i / (rows * 2.2))
        cursor = 2 + next(rnd) % 6
        while cursor < run:
            seg = 3 + next(rnd) % 6
            gap = 5 + next(rnd) % 8
            seg = min(seg, int(run - cursor))
            if seg <= 0:
                break
            x0 = x - cursor - seg if to_left else x + cursor
            px.rect(img, (x0, ry, x0 + seg, ry), fill=col)
            cursor += seg + gap


def chalk(img, name, x, y, size, color="cold.chalk", family="lucide"):
    """A Lucide outline rendered as if drawn on the board.

    Line icons and chalk are the same medium: a stroke of even weight with no
    fill. Rendering the library's geometry at the size it is shown, then
    thresholding, gives a chalk drawing without anyone drawing one.
    """
    ic = px.icon(name, size, color, family=family)
    px.paste(img, ic, x, y)
    return ic


# ---------------------------------------------------------------------------
# The splash: what shows once, in the middle of the screen, while the desktop
# session starts. One object, centred, and the credits underneath it.
# ---------------------------------------------------------------------------


def splash() -> Image.Image:
    bw, bh = BASE
    img = px.dither_v(bw, bh, "cold.void_lit", "cold.void", bands=10)
    starfield(img, (0, 0, bw, bh), density=110, seed=23)

    cx, cy = bw // 2, 112
    ramp = px.neon_ramp()

    # Two rings, the outer one turned so the colours do not line up and read as
    # one thick band. Both stop short of the top: the gap is drawn, not punched
    # out afterwards with a rectangle of background, which left a visible notch.
    neon_ring(img, cx, cy, 74, 3, ramp, start=288, sweep=324)
    neon_ring(img, cx, cy, 65, 2, ramp, start=300, sweep=300, alpha=200)

    # The power glyph sits in that gap.
    chalk(img, "power", cx - 11, cy - 84, 22, "cold.neon_cyan")

    sprite = mascot.sprite(scale=2, outline=True)
    px.paste(img, sprite, cx - sprite.width // 2, cy - sprite.height // 2 + 2)

    # Wordmark. Press Start 2P is on an eight-pixel grid, so a size that is a
    # multiple of eight lands on whole pixels and needs no cleanup.
    mark = px.text(brand("system.wordmark"), 24, "warm.screen_lit", px.FONT_PIXEL)
    mark = px.tint_ramp(mark, ramp[:6])
    mark = px.with_outline(mark, "mascot.outline", 1)
    mark = px.drop_shadow(mark, 1, 2, "#000000", 150)
    px.paste(img, mark, cx - mark.width // 2, 196)

    out = px.upscale(img, K)
    w, h = out.size

    # --- everything with letters in it, at screen resolution --------------
    #
    # Arabic is drawn here rather than on the small canvas for a reason worth
    # keeping: at eleven pixels the threshold that makes Latin crisp eats the
    # joins between Arabic letters and the word falls apart. Thirty pixels is
    # where Almarai Bold survives being reduced to on-or-off pixels, and this
    # is the only layer with thirty pixels to spend.
    tagline(out, cx * K, 908)

    px.draw_text(out, brand("author.repo"), w // 2, h - 106, 21, "cold.neon_cyan",
                 px.FONT_UI, anchor="mt")
    px.draw_text(out, f"built by {brand('author.name')}", w // 2, h - 76, 21,
                 "cold.chalk", px.FONT_UI, anchor="mt")

    if brand("support.url"):
        heart = px.icon("heart-fill", 18, "cold.neon_pink", family="phosphor")
        line = px.text(f"{brand('support.cta_en')}  ·  {brand('support.url')}", 21,
                       "cold.neon_amber", px.FONT_UI)
        total = heart.width + 10 + line.width
        x = (w - total) // 2
        px.paste(out, heart, x, h - 40)
        px.paste(out, line, x + heart.width + 10, h - 44)

    return px.crt(out, scan_period=3, scan_strength=0.06, bloom=4, bloom_strength=0.16,
                  vig=0.20)


def tagline(out, cx: int, y: int, size: int = 30) -> None:
    """`hajime · begin`: the name, then what it means.

    Latin only, and deliberately. Arabic is not painted into the boot pictures
    any more -- the console underneath them cannot shape it, and a system that
    speaks Arabic in a JPEG while its console speaks English is doing
    translation as decoration. The Arabic in this system comes from the locale,
    where the rest of it comes from.
    """
    name = px.text("hajime", size, "cold.chalk_dim", px.FONT_UI)
    dot = px.text("·", size, "cold.chalk_dim", px.FONT_UI)
    en = px.text(brand("system.tagline_en"), size, "cold.chalk", px.FONT_UI_BOLD)
    gap = size // 2
    total = name.width + gap + dot.width + gap + en.width
    x = cx - total // 2
    base = y + size
    px.paste(out, name, x, base - name.height)
    x += name.width + gap
    px.paste(out, dot, x, base - dot.height - 2)
    x += dot.width + gap
    px.paste(out, en, x, base - en.height)


# ---------------------------------------------------------------------------
# The loader board: a blackboard in a wooden frame. This is the picture the
# firmware shows while the menu is up, so everything on it is something true
# before the kernel has started -- who made the machine, where it lives, and how
# to keep it fed. No progress bar: nothing here can measure progress.
# ---------------------------------------------------------------------------


def board() -> Image.Image:
    bw, bh = BASE
    img = px.new(bw, bh, "cold.board")

    # --- the frame -------------------------------------------------------
    px.wood(img, (0, 0, bw - 1, 13), seed=3, grain=4)
    px.wood(img, (0, bh - 20, bw - 1, bh - 1), seed=9, grain=4)
    px.wood(img, (0, 0, 13, bh - 1), seed=5, grain=6)
    px.wood(img, (bw - 14, 0, bw - 1, bh - 1), seed=7, grain=6)
    px.rect(img, (0, 0, bw - 1, bh - 1), outline="warm.outline")
    px.rect(img, (13, 13, bw - 14, bh - 21), outline="warm.outline")
    for x, y in ((5, 5), (bw - 9, 5), (5, bh - 9), (bw - 9, bh - 9)):
        px.screw(img, x, y)

    # --- the board surface ----------------------------------------------
    inner = (14, 14, bw - 15, bh - 22)
    px.rect(img, inner, fill="cold.board")
    px.dither_fill(img, (inner[0], inner[1], inner[2], inner[3]), "cold.board",
                   "cold.board_dark", ratio=0.35)
    # Chalk dust, heavier towards the bottom where a cloth has been.
    rnd = lcg(31)
    d = px.draw_on(img)
    for _ in range(700):
        x = inner[0] + next(rnd) % (inner[2] - inner[0])
        y = inner[1] + next(rnd) % (inner[3] - inner[1])
        near_bottom = (y - inner[1]) / (inner[3] - inner[1])
        if (next(rnd) % 100) / 100 < near_bottom * 0.5:
            d.point((x, y), fill=px.C("cold.chalk", 26))

    # --- the mascot, mid-flight -----------------------------------------
    sprite = mascot.sprite(scale=2, outline=True)
    mx, my = 226, 50
    px.paste(img, sprite, mx, my)

    # A narrow band of falling bits between the writing and the robot. Narrow is
    # the point: the first version wandered across the text.
    # The falling bits are added after the enlargement: on the small canvas
    # the smallest legible digit is four screen pixels wide, which made a
    # column of them louder than the writing beside it.

    # --- chalk doodles ---------------------------------------------------
    # A skyline, a cloud over it, a cat, a sprout and a star: the margin of a
    # notebook, which is what the right side of a blackboard is.
    chalk(img, "cat", 352, 42, 24, "cold.chalk_dim")
    chalk(img, "cloud", 400, 50, 26, "cold.chalk_dim")
    chalk(img, "star", 440, 38, 18, "cold.neon_amber")
    chalk(img, "building-2", 386, 92, 30, "cold.chalk_dim")
    chalk(img, "building-2", 418, 102, 22, "cold.chalk_dim")
    chalk(img, "sprout", 356, 108, 22, "cold.chalk_dim")
    chalk(img, "bot", 424, 140, 20, "cold.chalk_dim")

    # --- the wordmark ----------------------------------------------------
    ramp = px.neon_ramp()
    mark = px.text(brand("system.wordmark"), 22, "warm.screen_lit", px.FONT_PIXEL)
    mark = px.tint_ramp(mark, ramp[:6])
    mark = px.with_outline(mark, "mascot.outline", 1)
    px.paste(img, mark, bw // 2 - mark.width // 2, 176)

    # --- the chalk rail --------------------------------------------------
    # Four sticks and a cloth on the bottom rail. The rail is the reason the
    # bottom of the frame is wider than the other three sides.
    for i, col in enumerate(("cold.chalk", "cold.neon_pink", "cold.neon_cyan",
                             "cold.neon_amber")):
        x = 300 + i * 26
        px.rect(img, (x, 254, x + 17, 258), fill=col, outline="warm.outline")
    px.rect(img, (392, 252, 420, 259), fill="warm.bezel_dark", outline="warm.outline")
    px.rect(img, (392, 252, 420, 254), fill="warm.bezel_light")

    out = px.upscale(img, K)
    w, h = out.size
    binary_rain(out, (600, 236, 848, 632), seed=17, cols=3, size=16)

    # --- the writing, at screen resolution -------------------------------
    lines = [
        ("cold.neon_lime", f"> {brand('system.name')} {brand('system.version')}"),
        ("cold.chalk", "> FreeBSD 14.4 · ZFS boot environments"),
        ("cold.chalk", "> a server that fits in 8 GB"),
        (None, ""),
        ("cold.neon_cyan", f"> {brand('author.repo')}"),
        ("cold.chalk", f"> built by {brand('author.name')}"),
    ]
    y = 122
    for color, s in lines:
        if s and color:
            px.draw_text(out, s, 108, y, 25, color, px.FONT_UI)
        y += 40

    tagline(out, w // 2, 812, size=28)

    # --- the greeting and the ask ----------------------------------------
    px.draw_text(out, brand("greeting.en"), w // 2, h - 232, 30, "cold.chalk",
                 px.FONT_UI_BOLD, anchor="mt")

    if brand("support.url"):
        heart = px.icon("heart-fill", 20, "cold.neon_pink", family="phosphor")
        ask = px.text(brand("support.ask_en"), 22, "cold.chalk", px.FONT_UI)
        url = px.text(brand("support.url"), 22, "cold.neon_amber", px.FONT_UI)
        block = max(ask.width, url.width)
        x = (w - (heart.width + 12 + block)) // 2
        px.paste(out, heart, x, h - 154)
        px.paste(out, ask, x + heart.width + 12, h - 158)
        px.paste(out, url, x + heart.width + 12, h - 128)

    return px.crt(out, scan_period=3, scan_strength=0.05, bloom=3, bloom_strength=0.12,
                  vig=0.18)
