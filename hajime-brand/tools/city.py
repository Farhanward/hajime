"""The wallpaper: a pixel city at night, with the machine building it.

Drawn in three depth bands, back to front, with nothing between them. Aerial
perspective in pixel art is a question of how few colours a band can get away
with: the far towers get two, the middle four, the near ones six. Blending the
bands into a continuous haze would spend the depth rather than build it.

The picture also has a job. It sits behind a file manager and a column of
launchers for hours at a time, so the loud parts -- the beacon, the billboard,
the train -- sit high or low, and the middle stays quiet enough to lose a window
against.
"""

from __future__ import annotations

from PIL import Image, ImageFilter

import mascot
import px
import scenes
from brand import brand

BASE = scenes.BASE
K = scenes.K

# The vertical plan, in base units. Everything below is expressed against these
# four numbers, so moving the horizon moves the city and not just one band.
FAR_BASE = 178          # where the back towers stand
MID_BASE = 214          # where the rust band stands
NEAR_TOP = 214          # the near roofs begin
RAIL_Y = 192            # the elevated track

BEACON_X = 428
BEACON_TOP = 44
BILLBOARD = (34, 116, 140, 152)
ROBOT = (322, 100)


# --- sky --------------------------------------------------------------------


def sky(img):
    bw, bh = BASE
    px.paste(img, px.dither_v(bw, bh, "city.sky_high", "city.sky_low", bands=14), 0, 0)
    scenes.starfield(img, (0, 4, bw, 140), density=130, seed=71)


def cloud(img, x, y, w, seed):
    """A cloud as stacked bars of two widths, lit on the side facing the beacon.

    Flat and hard-edged on purpose. A soft cloud in a picture where everything
    else has a one-pixel edge reads as a smudge on the glass.
    """
    rnd = scenes.lcg(seed)
    rows = [(x, w), (x + 4 + next(rnd) % 4, w - 10 - next(rnd) % 6),
            (x + 10 + next(rnd) % 6, w - 22 - next(rnd) % 8)]
    for i, (rx, rw) in enumerate(rows):
        if rw < 6:
            continue
        ry = y - i * 3
        px.rect(img, (rx, ry, rx + rw, ry + 3), fill="city.smog")
        px.hline(img, ry, rx + 1, rx + rw - 1, "#3a3468")
    lit = "cold.neon_cyan" if x + w // 2 < BEACON_X else "cold.neon_pink"
    px.vline(img, x + w if x + w // 2 < BEACON_X else x, y - 4, y + 3, lit)


def clouds(img):
    for i, (x, y, w) in enumerate(((26, 46, 54), (170, 30, 44), (300, 58, 38),
                                   (416, 36, 50))):
        cloud(img, x, y, w, seed=29 + i * 7)


# --- towers -----------------------------------------------------------------


def _grid_windows(img, x0, y0, x1, y1, rnd, cols=4, density=6, warm="city.lit_warm",
                  cool="city.lit_cool"):
    """Windows on a grid rather than scattered.

    A building's windows line up. Scattering them, which is what the first pass
    did, gives a rock face with holes in it.
    """
    span = x1 - x0
    if span < 6:
        return
    step = max(3, span // cols)
    for wy in range(y0, y1 - 1, 5):
        for wx in range(x0, x1 - 1, step):
            r = next(rnd) % density
            if r == 0:
                px.rect(img, (wx, wy, wx + 1, wy + 1), fill=warm)
            elif r == 1:
                px.rect(img, (wx, wy, wx + 1, wy + 1), fill=cool)


def far_towers(img, seed=41):
    """The back band: two colours, and one lit room in a dozen."""
    rnd = scenes.lcg(seed)
    x = -6
    while x < BASE[0]:
        w = 16 + next(rnd) % 22
        h = 34 + next(rnd) % 54
        top = FAR_BASE - h
        px.rect(img, (x, top, x + w, FAR_BASE), fill="city.far")
        px.hline(img, top, x, x + w, "city.far_lit")
        _grid_windows(img, x + 3, top + 5, x + w - 2, FAR_BASE - 4, rnd, cols=3,
                      density=13, warm="city.far_lit", cool="city.far_lit")
        x += w + 2 + next(rnd) % 7


def mid_towers(img, seed=13):
    """The rust band: the body of the city, and most of the light in it."""
    rnd = scenes.lcg(seed)
    x = -8
    while x < BASE[0]:
        w = 20 + next(rnd) % 26
        h = 38 + next(rnd) % 46
        top = MID_BASE - h
        if abs(x + w // 2 - BEACON_X) < 30:      # room for the beacon tower
            x += w + 3
            continue
        px.rect(img, (x, top, x + w, MID_BASE), fill="city.mid")
        px.hline(img, top, x, x + w, "city.mid_lit")
        px.vline(img, x, top, MID_BASE, "city.mid_dark")
        px.vline(img, x + w, top, MID_BASE, "city.mid_dark")
        # A vertical pilaster or two, so the face is not one flat rectangle.
        for px_x in range(x + 6, x + w - 4, 9):
            px.vline(img, px_x, top + 3, MID_BASE - 2, "city.mid_dark")

        crown = next(rnd) % 3
        if crown == 0:                            # a setback
            px.rect(img, (x + 4, top - 7, x + w - 4, top), fill="city.mid")
            px.hline(img, top - 7, x + 4, x + w - 4, "city.mid_lit")
        elif crown == 1:                          # a mast with a warning light
            tx = x + w // 2
            px.vline(img, tx, top - 13, top, "city.mid_dark")
            px.rect(img, (tx - 1, top - 15, tx + 1, top - 13), fill="cold.neon_pink")
        else:                                     # a tank
            px.rect(img, (x + 4, top - 6, x + 12, top), fill="city.mid_dark")
            px.hline(img, top - 6, x + 4, x + 12, "city.mid")

        _grid_windows(img, x + 3, top + 5, x + w - 3, MID_BASE - 3, rnd, cols=4,
                      density=5)
        x += w + 3 + next(rnd) % 7


def beacon_tower(img):
    """The tallest tower, wearing the logo's ring at the size a building wears one.

    It is the one unambiguously Hajime thing in the picture, so everything else
    leans away from it and nothing else is allowed the full spectrum.
    """
    w = 34
    x0, x1 = BEACON_X - w // 2, BEACON_X + w // 2
    px.rect(img, (x0, BEACON_TOP + 20, x1, MID_BASE + 2), fill="city.mid")
    px.hline(img, BEACON_TOP + 20, x0, x1, "city.mid_lit")
    px.vline(img, x0, BEACON_TOP + 20, MID_BASE, "city.mid_dark")
    px.vline(img, x1, BEACON_TOP + 20, MID_BASE, "city.mid_dark")
    for x in range(x0 + 7, x1 - 4, 8):
        px.vline(img, x, BEACON_TOP + 24, MID_BASE - 2, "city.mid_dark")
    _grid_windows(img, x0 + 3, BEACON_TOP + 26, x1 - 3, MID_BASE - 3,
                  scenes.lcg(97), cols=4, density=4)

    px.rect(img, (BEACON_X - 2, BEACON_TOP + 2, BEACON_X + 2, BEACON_TOP + 22),
            fill="city.mid_dark")
    ramp = px.neon_ramp()
    scenes.neon_ring(img, BEACON_X, BEACON_TOP - 12, 17, 2, ramp, start=288, sweep=324)
    scenes.neon_ring(img, BEACON_X, BEACON_TOP - 12, 11, 1, ramp, start=300, sweep=300,
                     alpha=190)
    px.paste(img, px.icon("power", 14, "cold.neon_cyan"), BEACON_X - 7, BEACON_TOP - 30)


def billboard(img, box):
    """A lit sign on a building's flank. Its writing goes on at full scale."""
    x0, y0, x1, y1 = box
    px.rect(img, (x0 - 3, y0 - 3, x1 + 3, y1 + 3), fill="city.mid_dark")
    px.rect(img, box, fill="#0d1026", outline="city.rail")
    for sx in range(x0 + 3, x1 - 2, 7):
        px.rect(img, (sx, y0 - 5, sx + 1, y0 - 4), fill="city.lit_warm")
    for lx in (x0 + 6, x1 - 8):
        px.rect(img, (lx, y1 + 3, lx + 2, y1 + 22), fill="city.mid_dark")


# --- the railway ------------------------------------------------------------


def railway(img, seed=53):
    """An elevated line that steps down as it crosses, on piers, and stops.

    A straight rule from edge to edge -- the first attempt -- cuts the picture in
    half. Steps and an ending let it belong to the city instead of framing it.
    """
    rnd = scenes.lcg(seed)
    segments = [(0, RAIL_Y), (86, RAIL_Y), (86, RAIL_Y + 6), (196, RAIL_Y + 6),
                (196, RAIL_Y + 12), (268, RAIL_Y + 12)]
    for i in range(0, len(segments) - 1, 2):
        (x0, y0), (x1, _y1) = segments[i], segments[i + 1]
        px.rect(img, (x0, y0, x1, y0 + 2), fill="city.rail")
        px.hline(img, y0, x0, x1, "city.rail_lit")
        px.hline(img, y0 + 3, x0, x1, "city.mid_dark")
        for x in range(x0 + 8, x1, 30):
            px.rect(img, (x, y0 + 4, x + 2, MID_BASE), fill="city.mid_dark")
            px.vline(img, x, y0 + 4, MID_BASE - 2, "city.rail")
            for k in range(2, 14, 3):             # a brace, angled
                px.rect(img, (x + k, y0 + 4 + k, x + k + 1, y0 + 5 + k),
                        fill="city.mid_dark")
        if i + 2 < len(segments):                 # the step down to the next run
            nx, ny = segments[i + 2]
            px.rect(img, (nx - 2, y0, nx + 2, ny + 2), fill="city.rail")
    _ = rnd


def train(img, x, rail_y, cars=3):
    """A train, lit from inside, with a lamp at the front."""
    y = rail_y - 11
    for c in range(cars):
        cx = x + c * 25
        px.rect(img, (cx, y, cx + 22, y + 10), fill="city.rail",
                outline="warm.outline")
        px.rect(img, (cx + 1, y + 1, cx + 21, y + 2), fill="city.rail_lit")
        for wx in range(cx + 3, cx + 20, 5):
            px.rect(img, (wx, y + 3, wx + 3, y + 6), fill="city.lit_cool")
        px.rect(img, (cx + 2, y + 8, cx + 20, y + 9), fill="city.mid_dark")
    px.rect(img, (x - 5, y + 3, x - 1, y + 6), fill="cold.neon_amber")


# --- foreground -------------------------------------------------------------


def rooftops(img, seed=67):
    """The near band: roofs almost in shadow, and the two living things on them."""
    bw, bh = BASE
    px.rect(img, (0, NEAR_TOP, bw - 1, bh - 1), fill="city.near")

    rnd = scenes.lcg(seed)
    x = -4
    roofs = []
    while x < bw:
        w = 30 + next(rnd) % 44
        drop = 8 + next(rnd) % 26
        top = NEAR_TOP + drop
        px.rect(img, (x, top, x + w, bh - 1), fill="city.near")
        px.hline(img, top, x, x + w, "city.near_lit")
        px.vline(img, x + w, top, bh - 1, "#241713")
        _grid_windows(img, x + 4, top + 6, x + w - 4, bh - 4, rnd,
                      cols=3, density=9, warm="#c88f3a", cool="#3f7e93")
        roofs.append((x, top, x + w))

        kind = next(rnd) % 4
        fx = x + 8 + next(rnd) % max(1, w - 26)
        if kind == 0:                                    # a vent block
            px.rect(img, (fx, top - 7, fx + 10, top), fill="city.near")
            px.hline(img, top - 7, fx, fx + 10, "city.mid")
        elif kind == 1:                                  # an aerial
            px.vline(img, fx, top - 14, top, "city.near")
            px.rect(img, (fx - 3, top - 14, fx + 3, top - 13), fill="city.near")
            px.rect(img, (fx - 1, top - 17, fx + 1, top - 15), fill="cold.neon_pink")
        elif kind == 2:                                  # a water tank on legs
            px.rect(img, (fx, top - 9, fx + 12, top - 3), fill="city.near_lit")
            px.hline(img, top - 9, fx, fx + 12, "city.mid")
            px.vline(img, fx + 2, top - 3, top, "city.near_lit")
            px.vline(img, fx + 10, top - 3, top, "city.near_lit")
        else:                                            # a lit stairwell door
            px.rect(img, (fx, top - 8, fx + 6, top), fill="city.near")
            px.rect(img, (fx + 2, top - 6, fx + 4, top - 1), fill="city.lit_warm")
        x += w + 2

    # A cat on a parapet and a plant in a pot: the only two living things here,
    # and neither is drawn by hand -- both are filled icons from Phosphor, edged
    # so they read against the roof they sit on.
    if len(roofs) > 2:
        rx, ry, _ = roofs[1]
        cat = px.with_outline(px.icon("cat-fill", 16, "city.mid_lit", family="phosphor"),
                              "#241713", 1)
        px.paste(img, cat, rx + 18, ry - 16)
    if len(roofs) > 4:
        rx, ry, rx1 = roofs[-2]
        plant = px.with_outline(
            px.icon("potted-plant-fill", 18, "#4e7a4a", family="phosphor"),
            "#241713", 1)
        px.paste(img, plant, min(rx + 20, rx1 - 22), ry - 18)


# --- the machine, and what it projects --------------------------------------


def holo_window(img, box, rows=4):
    """A window as a projection: an outline, a lit bar, lines of nothing.

    Deliberately unlike the real window chrome. The wallpaper's windows are
    light in the air; the desktop's are objects on a surface. Drawing them alike
    would leave someone clicking at a picture.
    """
    x0, y0, x1, y1 = box
    panel = px.new(x1 - x0 + 1, y1 - y0 + 1)
    px.rect(panel, (0, 0, x1 - x0, y1 - y0), fill="#0e2338")
    px.rect(panel, (0, 0, x1 - x0, y1 - y0), outline="city.holo")
    px.rect(panel, (1, 1, x1 - x0 - 1, 3), fill="city.holo")
    for i in range(rows):
        ry = 7 + i * 5
        if ry > y1 - y0 - 3:
            break
        px.rect(panel, (3, ry, x1 - x0 - 4 - (i % 3) * 7, ry + 1), fill="city.rail_lit")
    # Projections are see-through; that is most of what makes them read as
    # projections rather than as windows someone forgot to close.
    panel.putalpha(panel.getchannel("A").point(lambda v: int(v * 0.72)))
    px.paste(img, panel, x0, y0)
    for cx, cy in ((x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)):
        px.rect(img, (cx - 1, cy - 1, cx + 1, cy + 1), fill="cold.neon_cyan")


def beam(img, x0, y0, x1, y1, seed=5):
    """A stream of ones and zeros running from a hand to a projection."""
    rnd = scenes.lcg(seed)
    steps = max(abs(x1 - x0), abs(y1 - y0))
    for i in range(0, steps, 3):
        t = i / max(1, steps)
        x = round(x0 + (x1 - x0) * t)
        y = round(y0 + (y1 - y0) * t) + (next(rnd) % 3) - 1
        px.rect(img, (x, y, x + 1, y), fill="city.holo")
    for i in range(3):
        t = 0.25 + i * 0.25
        x = round(x0 + (x1 - x0) * t)
        y = round(y0 + (y1 - y0) * t)
        digit = "01"[(next(rnd) >> 5) & 1]
        px.paste(img, px.text(digit, 8, "cold.neon_cyan", px.FONT_PIXEL),
                 x - 2, y - 7 + (next(rnd) % 3))


def machine(img):
    """The robot, hovering, and the interfaces coming out of its hands.

    Doing something rather than posing: the streams leave its hands and end at
    the projections, so the picture has a subject and a verb.
    """
    sprite = mascot.sprite(scale=2, outline=True)
    mx, my = ROBOT
    px.paste(img, sprite, mx, my)

    # A pad of light under it, so it reads as hovering rather than falling.
    cx = mx + sprite.width // 2
    for i, w in enumerate((26, 18, 10)):
        px.rect(img, (cx - w, my + sprite.height + i * 3, cx + w,
                      my + sprite.height + i * 3), fill="city.holo")

    holo_window(img, (236, 58, 306, 104), rows=5)
    holo_window(img, (414, 150, 474, 188), rows=4)
    beam(img, mx + 6, my + 40, 306, 88, seed=11)
    beam(img, mx + 82, my + 86, 414, 158, seed=23)


# --- assembly ---------------------------------------------------------------


def bloom(img: Image.Image, radius: int = 8, strength: float = 0.42,
          floor: int = 150, saturation: int = 70) -> Image.Image:
    """Volumetric glow, on the neon only.

    Brightness alone is the wrong test. The mascot's silver is brighter than the
    cyan beside it, so a brightness threshold blew the robot out to a white
    smear while the signs it was standing under barely lifted. A light in this
    picture is *coloured*: bright and far from grey. Gating on the spread
    between a pixel's channels as well as its level leaves the metal alone and
    lets the signs burn.
    """
    src = img.convert("RGB")
    w, h = src.size
    lights = Image.new("RGB", (w, h))
    sp, lp = src.load(), lights.load()
    for y in range(h):
        for x in range(w):
            r, g, b = sp[x, y]
            hi, lo = max(r, g, b), min(r, g, b)
            lp[x, y] = (r, g, b) if hi >= floor and hi - lo >= saturation else (0, 0, 0)
    lights = lights.filter(ImageFilter.GaussianBlur(radius))
    out = Image.new("RGBA", src.size)
    base, lit, dst = src.load(), lights.load(), out.load()
    for y in range(src.height):
        for x in range(src.width):
            br, bg, bb = base[x, y]
            lr, lg, lb = lit[x, y]
            dst[x, y] = (min(255, br + int(lr * strength)),
                         min(255, bg + int(lg * strength)),
                         min(255, bb + int(lb * strength)), 255)
    return out


def base_image() -> Image.Image:
    bw, bh = BASE
    img = px.new(bw, bh)
    sky(img)
    clouds(img)
    far_towers(img)
    beacon_tower(img)
    mid_towers(img)
    billboard(img, BILLBOARD)
    railway(img)
    train(img, 104, RAIL_Y + 6)
    rooftops(img)
    machine(img)
    return img


def billboard_text(out):
    """The sign's writing: the wordmark, and one line under it.

    This Arabic line is the only Arabic baked into any picture in the system.
    Everywhere else the interface says what it has to say through the locale,
    the way a system should. A painted sign on a building is not an interface.
    """
    x0, y0, x1, _y1 = BILLBOARD
    cx = (x0 + x1) // 2 * K
    mark = px.upscale(px.tint_ramp(
        px.text(brand("system.wordmark"), 11, "warm.screen_lit", px.FONT_PIXEL),
        px.neon_ramp()[:6]), 2)
    px.paste(out, mark, cx - mark.width // 2, (y0 + 7) * K)
    px.draw_text(out, brand("greeting.ar"), cx, (y0 + 22) * K, 17, "#c8d2ff",
                 px.FONT_AR, anchor="mt")


def wallpaper(crt: bool = True) -> Image.Image:
    out = px.upscale(base_image(), K)
    billboard_text(out)
    out = bloom(out, strength=0.34)
    if not crt:
        return out
    return px.crt(out, scan_period=3, scan_strength=0.05, bloom=2,
                  bloom_strength=0.05, vig=0.16)
