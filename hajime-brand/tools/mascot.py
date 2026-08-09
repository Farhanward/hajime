"""The mascot, drawn rather than stored.

Every other picture in this system comes from an open icon set, because an
interface icon that means "folder" has already been drawn better by people who
draw icons. A mascot is the exception: it is the one image that has to be this
system's and no other's, so it is built here out of primitives, at a size a
sprite would actually be.

It is written as code and not as a stored image for a practical reason. The
robot appears at six sizes, from a 16-pixel favicon to a quarter of a boot
screen, and each of those wants different detail. Scaling one bitmap up gives
mush and scaling it down gives noise; regenerating it at each size gives a
sprite that is correct at every one of them.

Grid: 36 x 44, one unit per pixel. Everything is expressed in that grid, so
changing a proportion means changing a number here and not repainting.
"""

from __future__ import annotations

from PIL import Image

import px

W, H = 44, 52

O = "mascot.outline"
SIL = "mascot.silver"
LIT = "mascot.silver_lit"
DIM = "mascot.silver_dim"
DRK = "mascot.silver_dark"
VIS = "mascot.visor"
VISL = "mascot.visor_lit"
EYE = "mascot.eye"
ACC = "mascot.accent"
ACCD = "mascot.accent_dark"
TRIM = "mascot.trim"
CHEEK = "mascot.cheek"


def _rr(img, box, fill, radius=2, outline=O, width=1):
    px.draw_on(img).rounded_rectangle(box, radius=radius, fill=px.C(fill),
                                      outline=px.C(outline) if outline else None, width=width)


def _chevron(img, cx, cy, reach: int, thick: int, color):
    """A bold ∧ with its apex at (cx, cy).

    Drawn pixel by pixel rather than with two lines: a diagonal line primitive
    at this size lands its endpoints where it likes, and an eye that is one
    pixel off centre is a squint.
    """
    d = px.draw_on(img)
    for i in range(reach + 1):
        for t in range(thick):
            d.point((cx - i, cy + i + t), fill=px.C(color))
            d.point((cx + i, cy + i + t), fill=px.C(color))


def build(wave: int = 0, arms: bool = True) -> Image.Image:
    """The sprite at 1:1. `wave` lifts the raised fist by that many pixels.

    A single parameter is enough for the animation the splash needs: one frame
    with the fist up, one with it a pixel lower, and the robot is alive. More
    would be a character rig for a picture that shows for four seconds.

    Drawing order is back to front: antenna, legs, torso, head, then the arms
    last so a raised arm passes in front of the body instead of vanishing behind
    it. Every limb starts inside the part it hangs from, because a one-pixel gap
    at a shoulder is the difference between a robot and a pile of parts.
    """
    img = px.new(W, H)
    lift = wave

    # --- antenna ---------------------------------------------------------
    # Stem before the helmet so the helmet's own outline closes over its base.
    px.rect(img, (21, 5, 23, 13), fill=DIM, outline=None)
    px.rect(img, (21, 5, 21, 13), fill=SIL, outline=None)
    _rr(img, (19, 0, 26, 7), EYE, radius=3)
    px.rect(img, (21, 2, 22, 2), fill=LIT, outline=None)

    # --- legs ------------------------------------------------------------
    # Four pixels of leg have to survive below the torso. Without them the
    # sprite is a head on a box and the boots read as its feet directly.
    _rr(img, (17, 40, 22, 51), SIL, radius=2)
    _rr(img, (22, 40, 27, 51), SIL, radius=2)
    _rr(img, (15, 47, 23, 51), ACC, radius=2)                    # boots, splayed
    _rr(img, (21, 47, 29, 51), ACCD, radius=2)
    px.rect(img, (17, 49, 19, 49), fill=EYE, outline=None)       # a light on each toe
    px.rect(img, (24, 49, 26, 49), fill=EYE, outline=None)

    # --- torso -----------------------------------------------------------
    px.rect(img, (19, 29, 25, 34), fill=DIM, outline=O)          # neck
    _rr(img, (14, 33, 30, 43), SIL, radius=5)
    px.draw_on(img).line([(16, 36), (16, 41)], fill=px.C(LIT))
    px.draw_on(img).line([(28, 36), (28, 41)], fill=px.C(DIM))
    px.rect(img, (16, 42, 28, 42), fill=DIM, outline=None)       # the hip seam

    # The badge. An H legible on an eight-pixel square has to be three strokes
    # placed by hand; a font at this size gives a grey smudge.
    _rr(img, (18, 34, 26, 41), ACC, radius=2)
    d = px.draw_on(img)
    d.rectangle((20, 36, 21, 39), fill=px.C(LIT))
    d.rectangle((23, 36, 24, 39), fill=px.C(LIT))
    d.rectangle((21, 37, 23, 38), fill=px.C(LIT))

    # --- head ------------------------------------------------------------
    # Wider than tall and sitting low. A tall helmet reads as a person in a hat.
    _rr(img, (9, 10, 35, 31), SIL, radius=7)
    px.draw_on(img).line([(14, 11), (30, 11)], fill=px.C(LIT))   # the room, above
    px.draw_on(img).line([(12, 29), (32, 29)], fill=px.C(DIM))   # and the shade below

    # The trim stripe: the one warm colour on a cold robot, and at sixteen
    # pixels the only feature left besides the visor.
    px.rect(img, (12, 14, 32, 16), fill=TRIM, outline=None)
    px.rect(img, (12, 14, 32, 14), fill="#ffc27a", outline=None)

    # --- visor -----------------------------------------------------------
    _rr(img, (11, 18, 33, 29), VIS, radius=4)
    px.rect(img, (14, 19, 30, 19), fill=VISL, outline=None)      # glass catching the room
    _chevron(img, 17, 21, 3, 2, EYE)
    _chevron(img, 27, 21, 3, 2, EYE)
    px.rect(img, (16, 22, 18, 22), fill="mascot.eye_core", outline=None)
    px.rect(img, (26, 22, 28, 22), fill="mascot.eye_core", outline=None)
    px.rect(img, (13, 26, 15, 26), fill=CHEEK, outline=None)
    px.rect(img, (29, 26, 31, 26), fill=CHEEK, outline=None)

    # --- arms ------------------------------------------------------------
    # Skipped when only the head is wanted: the raised fist passes within a
    # pixel of the helmet, so cropping it out afterwards takes a bite out of the
    # helmet too.
    if not arms:
        return img

    # In front of everything. The raised fist is the whole reason this robot
    # reads as glad to see you rather than merely present.
    # The raised arm clears the helmet rather than crossing it: silver on silver
    # with a one-pixel edge between them is a shape that disappears at any size
    # below this one.
    # Three shapes, not four. An earlier version split the forearm into two
    # offset rectangles to suggest a bend; at twice this size the two rounded
    # ends read as beads and the arm became a string of them.
    _rr(img, (9, 30 - lift, 15, 41), SIL, radius=2)              # upper arm
    _rr(img, (6, 20 - lift, 11, 32 - lift), SIL, radius=2)       # forearm
    _rr(img, (3, 14 - lift, 11, 22 - lift), DIM, radius=4)       # fist
    px.rect(img, (5, 16 - lift, 6, 17 - lift), fill=LIT, outline=None)
    px.rect(img, (5, 19 - lift, 8, 19 - lift), fill=DRK, outline=None)   # knuckle line

    _rr(img, (28, 34, 33, 44), SIL, radius=2)                    # arm down
    _rr(img, (29, 43, 36, 50), DIM, radius=3)                    # fist
    px.rect(img, (31, 45, 32, 45), fill=LIT, outline=None)
    px.rect(img, (31, 47, 34, 47), fill=DRK, outline=None)

    return img


def sprite(scale: int = 1, wave: int = 0, outline: bool = False) -> Image.Image:
    """The mascot at an integer scale, optionally with a heavy outer edge.

    The outer edge is for placing it on a busy background -- a chalkboard, a
    wallpaper -- where a one-pixel line disappears.
    """
    img = build(wave=wave)
    if outline:
        img = px.with_outline(img, O, 1)
    return px.upscale(img, scale) if scale > 1 else img


def head(scale: int = 1) -> Image.Image:
    """Just the helmet and visor, for the places a whole robot will not fit.

    The favicon, the panel button and the console's title mark are all sixteen
    pixels or so. A full body at that size is a smudge with legs; the head alone
    still reads as this robot.
    """
    # The crop follows the helmet box drawn in `build`, plus the antenna and two
    # pixels of air. It is written as an offset from those numbers rather than
    # as its own set: an earlier version held a crop from a previous geometry and
    # quietly returned a picture of the robot's elbow.
    full = build(arms=False)
    crop = full.crop((8, 0, 36, 33))
    return px.upscale(crop, scale) if scale > 1 else crop
