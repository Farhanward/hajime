"""The desktop's parts: windows, panel, buttons, desktop icons.

This module is drawn twice over. Once here, in pixels, to make the picture that
shows what the desktop looks like. And once in hajime-wm/hajime_theme.css, in
GTK's stylesheet language, to make the desktop actually look like it. The two
have to agree, so the measurements live in one place -- the constants below --
and the stylesheet quotes them.

What GTK can reproduce exactly: the colours, the square corners, the black
one-pixel edge, the two-tone bevel, the brass title bar, the sidebar. What it
cannot: the pixel grid itself. GTK draws at the display's resolution, so its
edges are one screen pixel where these are four. The picture is therefore the
intent at four times the detail, not a screenshot of the result.
"""

from __future__ import annotations

import px

# Measurements, in base units. One base unit is four screen pixels at 1080p.
TITLE_H = 11
BORDER = 1
BUTTON = 7
PANEL_H = 18
ICON = 22


def window(img, box, active: bool = True):
    """A window: black edge, brass title bar, bevelled cream body.

    Returns the title bar and body rectangles in base units so the caller can
    put text in them at screen resolution, where small type is legible.
    """
    x0, y0, x1, y1 = box
    bar = (x0, y0, x1, y0 + TITLE_H)
    body = (x0, y0 + TITLE_H, x1, y1)

    # Body first, then the bar on top of it: the bar's lower edge is the body's
    # upper one and drawing it twice leaves a seam.
    px.rect(img, (x0, y0, x1, y1), fill="warm.screen_lit", outline="warm.outline")
    px.bevel(img, (x0 + 1, y0 + TITLE_H, x1 - 1, y1 - 1), "warm.screen_lit", "warm.screen_dim")

    if active:
        px.brass(img, bar)
    else:
        px.rect(img, bar, fill="warm.bezel_dark", outline="warm.outline")
        px.bevel(img, (x0 + 1, y0 + 1, x1 - 1, y0 + TITLE_H - 1),
                 "warm.bezel", "warm.bezel_dark")
    px.hline(img, y0 + TITLE_H, x0, x1, "warm.outline")

    # Window controls. Three keys at the right, the close one blushed, each with
    # its own bevel so it reads as something to press rather than a coloured
    # patch. Their glyphs are drawn, not set: at six pixels a font has no room.
    bx = x1 - 3
    for i, tone in enumerate(("warm.screen_lit", "warm.screen_lit", "warm.blush")):
        bx -= BUTTON
        top = y0 + (TITLE_H - BUTTON) // 2
        px.rect(img, (bx, top, bx + BUTTON - 1, top + BUTTON - 1), fill=tone,
                outline="warm.outline")
        px.bevel(img, (bx + 1, top + 1, bx + BUTTON - 2, top + BUTTON - 2),
                 "#ffffff", "warm.screen_dim")
        gx, gy = bx + 2, top + 2
        d = px.draw_on(img)
        if i == 0:                                    # minimise: a bar at the foot
            px.rect(img, (gx, gy + 3, gx + 3, gy + 3), fill="warm.ink")
        elif i == 1:                                  # maximise: a hollow square
            px.rect(img, (gx, gy, gx + 3, gy + 3), outline="warm.ink")
            px.hline(img, gy + 1, gx, gx + 3, "warm.ink")
        else:                                         # close: a cross
            for k in range(4):
                d.point((gx + k, gy + k), fill=px.C("warm.ink"))
                d.point((gx + 3 - k, gy + k), fill=px.C("warm.ink"))
        bx -= 2

    return bar, body


def filled_icon(img, x, y, name, size, fill="warm.bezel", outline="warm.outline",
                family="phosphor"):
    """A solid icon with a dark edge: what a pixel-art icon actually is.

    The outline versions read well on a tile at desktop size, but inside a file
    manager, at twenty pixels, an outline folder is four thin strokes and a hole.
    Phosphor ships filled weights of the same shapes, so the fill comes from the
    library too rather than from someone painting inside the lines.
    """
    glyph = px.icon(name, size, fill, family=family)
    glyph = px.with_outline(glyph, outline, 1)
    px.paste(img, glyph, x, y)
    return glyph


def toolbar(img, box, glyphs=()):
    """A window's tool strip: a recessed rail with a few keys on it.

    Each key carries a glyph from the icon set. Empty keys read as a row of
    blank tiles, which is what the first draft of this looked like.
    """
    x0, y0, x1, y1 = box
    px.rect(img, box, fill="warm.screen")
    px.bevel(img, box, "warm.screen_dim", "warm.screen_lit", inset=True)
    for i, (name, family) in enumerate(glyphs):
        bx = x0 + 3 + i * 12
        if bx + 10 > x1:
            break
        button(img, (bx, y0 + 1, bx + 10, y1 - 1))
        glyph = px.icon(name, 7, "warm.ink", family=family)
        px.paste(img, glyph, bx + 2, y0 + 3)
    px.hline(img, y1, x0, x1, "warm.outline")


SIDEBAR_ROW = 14
SIDEBAR_PLACES = [
    ("house-fill", "الرئيسية", "Home"),
    ("hard-drives-fill", "المستودع", "Vault"),
    ("folder-fill", "المشاريع", "Projects"),
    ("floppy-disk-fill", "اللقطات", "Snapshots"),
    ("trash-fill", "المهملات", "Trash"),
]


def sidebar(img, box, selected: int = 1):
    """The places list down one side of a window.

    A sidebar rather than a row of tabs, and the one part of this layout that is
    not from 1995. Returns the row rectangles so the caller can put the labels in
    them at screen resolution.
    """
    x0, y0, x1, y1 = box
    px.rect(img, box, fill="warm.bezel")
    px.dither_fill(img, (x0, y0, x1, y1), "warm.bezel", "warm.bezel_dark", ratio=0.22)
    px.vline(img, x1, y0, y1, "warm.outline")
    rows = []
    for i, (icon_name, _ar, _en) in enumerate(SIDEBAR_PLACES):
        ry = y0 + 2 + i * SIDEBAR_ROW
        if ry + SIDEBAR_ROW > y1:
            break
        if i == selected:
            px.rect(img, (x0 + 1, ry, x1 - 1, ry + SIDEBAR_ROW - 2),
                    fill="warm.bezel_dark")
            px.hline(img, ry, x0 + 1, x1 - 1, "warm.bezel_light")
        filled_icon(img, x0 + 3, ry + 2, icon_name, 9, "warm.screen_lit")
        rows.append((x0, ry, x1, ry + SIDEBAR_ROW - 2))
    return rows


def button(img, box, label_space: bool = True, tone: str = "warm.screen_lit",
           pressed: bool = False):
    px.rect(img, box, fill=tone, outline="warm.outline")
    x0, y0, x1, y1 = box
    px.bevel(img, (x0 + 1, y0 + 1, x1 - 1, y1 - 1), "#ffffff", "warm.screen_dim",
             inset=pressed)
    _ = label_space


def panel(img, width, height, y):
    """The strip along the bottom: brass, with a lip of light along its top edge.

    It is the same material as a window's title bar on purpose. One machine,
    one set of parts.
    """
    px.brass(img, (0, y, width - 1, y + height - 1))
    px.hline(img, y, 0, width - 1, "warm.outline")
    px.hline(img, y + 1, 0, width - 1, "warm.bezel_light")
    _ = height


def gauge(img, cx, cy, r=6, value=0.7):
    """A round meter, as on the front of something with a power switch.

    The panel has three. They are decoration in this picture; in the running
    desktop the same three are the load, the memory and the temperature, which
    is why they are drawn as instruments and not as bar charts.
    """
    px.draw_on(img).ellipse((cx - r, cy - r, cx + r, cy + r), fill=px.C("warm.screen_lit"),
                            outline=px.C("warm.outline"))
    import math

    ang = math.radians(180 + value * 180)
    px.draw_on(img).line(
        [(cx, cy), (cx + (r - 2) * math.cos(ang), cy + (r - 2) * math.sin(ang))],
        fill=px.C("warm.bad"),
    )
    px.draw_on(img).point((cx, cy), fill=px.C("warm.ink"))


def desktop_icon(img, x, y, name, size=ICON, tile: bool = True):
    """A launcher on the desktop: a bevelled tile with a Lucide glyph on it.

    The tile matters. A bare glyph on a dark wallpaper is a smudge; a lit square
    under it gives the icon an edge and makes the column read as a column.
    """
    if tile:
        px.rect(img, (x, y, x + size, y + size), fill="warm.screen_lit",
                outline="warm.outline")
        px.bevel(img, (x + 1, y + 1, x + size - 1, y + size - 1), "#ffffff",
                 "warm.screen_dim")
    glyph = px.icon(name, size - 6, "warm.ink")
    px.paste(img, glyph, x + 3, y + 3)
