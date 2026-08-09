"""The picture of the desktop wearing this theme.

The wallpaper it stands on is drawn in city.py and installed as an asset. What
is added here -- launchers, two windows, the panel -- is drawn by wayfire and GTK
on the real machine, from the same palette and the same measurements in ui.py.

There is no television around it. A monitor painted onto the screen would be a
second monitor inside the real one. What makes this read as a machine from that
era is the scanline structure and the phosphor bloom laid over the whole surface
at the end, which is what a tube does to any picture put through it.

GTK's edges are one screen pixel where these are four, so treat this as the
intent at four times the detail, not a screenshot of the result.
"""

from __future__ import annotations

from PIL import Image

import city
import px
import scenes
import ui

BASE = scenes.BASE
K = scenes.K

def launchers() -> list[tuple[str, str, str]]:
    """The launchers down the left edge, read from the launchers themselves.

    This list used to be typed here, and it named six applications while the
    installer shipped three: a picture of a desktop with a paint program on it
    that nothing would ever open. Reading ../desktop/*.desktop instead means the
    artwork cannot promise something the machine does not have -- add a launcher
    and it appears, remove one and it goes.

    Both languages come from the same file the panel reads at run time, so what
    the picture shows in Arabic is what the Arabic session will show.
    """
    found = []
    for path in sorted((scenes.px.ROOT / "desktop").glob("*.desktop")):
        fields = {}
        for line in path.read_text(encoding="utf-8").splitlines():
            if "=" in line and not line.startswith("#"):
                key, _, value = line.partition("=")
                fields[key.strip()] = value.strip()
        icon = fields.get("X-Hajime-Icon")
        name = fields.get("Name")
        name_ar = fields.get("Name[ar]")
        if icon and name and name_ar:
            found.append((icon, name_ar, name))
    return found


LAUNCHERS = launchers()

# Both windows sit left of centre so the robot on the wallpaper stays visible.
# A picture of a desktop that buries the wallpaper's subject is a picture of a
# wallpaper nobody can see.
FILES_WIN = (62, 26, 246, 166)
NOTES_WIN = (104, 100, 300, 230)

FOLDERS = [
    ("المشاريع", "projects"),
    ("النسخ", "backups"),
    ("السجلات", "logs"),
    ("الوسائط", "media"),
    ("الإعداد", "etc"),
    ("المواقع", "www"),
]


def wallpaper(crt: bool = True) -> Image.Image:
    """The installed wallpaper, unchanged. Here so callers have one door."""
    return city.wallpaper(crt=crt)


def desktop() -> Image.Image:
    bw, bh = BASE
    img = city.base_image()

    # --- launchers -------------------------------------------------------
    for i, (name, _ar, _en) in enumerate(LAUNCHERS):
        ui.desktop_icon(img, 14, 20 + i * 34, name, size=ui.ICON)

    # --- the file manager, behind and inactive ---------------------------
    # Two windows and not one: the inactive title bar is half of a window theme
    # and a single window never shows it.
    fx0, fy0, fx1, fy1 = FILES_WIN
    ui.window(img, FILES_WIN, active=False)
    ui.toolbar(img, (fx0 + 1, fy0 + ui.TITLE_H + 1, fx1 - 1, fy0 + ui.TITLE_H + 12),
               glyphs=(("chevron-right", "lucide"), ("house-fill", "phosphor"),
                       ("folder-fill", "phosphor"), ("search", "lucide")))
    ui.sidebar(img, (fx0 + 1, fy0 + ui.TITLE_H + 13, fx0 + 56, fy1 - 1), selected=2)
    for i in range(len(FOLDERS)):
        col, row = i % 3, i // 3
        ui.filled_icon(img, fx0 + 66 + col * 40, fy0 + ui.TITLE_H + 20 + row * 48,
                       "folder-fill", 22, "warm.bezel")

    # --- the notes window, in front --------------------------------------
    nx0, ny0, nx1, ny1 = NOTES_WIN
    ui.window(img, NOTES_WIN, active=True)
    ui.toolbar(img, (nx0 + 1, ny0 + ui.TITLE_H + 1, nx1 - 1, ny0 + ui.TITLE_H + 12),
               glyphs=(("floppy-disk-fill", "phosphor"), ("file-text-fill", "phosphor"),
                       ("search", "lucide")))
    px.rect(img, (nx0 + 3, ny0 + ui.TITLE_H + 15, nx1 - 3, ny1 - 3),
            fill="warm.screen_lit", outline="warm.outline")
    px.bevel(img, (nx0 + 4, ny0 + ui.TITLE_H + 16, nx1 - 4, ny1 - 4),
             "warm.screen_dim", "#ffffff", inset=True)

    # --- the panel -------------------------------------------------------
    py = bh - ui.PANEL_H
    ui.panel(img, bw, ui.PANEL_H, py)
    ui.button(img, (4, py + 3, 48, py + 15))
    px.rect(img, (8, py + 7, 11, py + 10), fill="warm.bad", outline="warm.outline")
    for i, name in enumerate(("folder-fill", "terminal-window-fill", "globe-fill")):
        ui.filled_icon(img, 56 + i * 18, py + 4, name, 11, "warm.screen_lit")
    for i, v in enumerate((0.62, 0.38, 0.52)):
        ui.gauge(img, 352 + i * 22, py + 9, r=6, value=v)

    out = px.upscale(img, K)

    # The sign's writing follows the sign. With a window over it, the letters
    # would be the one thing on the wallpaper that ignores the stacking order.
    bx0, by0, bx1, by1 = city.BILLBOARD
    hidden = any(bx0 < wx1 and bx1 > wx0 and by0 < wy1 and by1 > wy0
                 for wx0, wy0, wx1, wy1 in (FILES_WIN, NOTES_WIN))
    if not hidden:
        city.billboard_text(out)

    # --- everything with letters, at screen resolution --------------------
    for i, (_name, ar, en) in enumerate(LAUNCHERS):
        y = (20 + i * 34 + ui.ICON) * K
        # White on a lit city needs its own edge, not a one-pixel shadow: the
        # label crosses a window, a roof and a neon sign in the same word.
        px.draw_text(out, ar, 25 * K, y + 4, 20, "#ffffff", px.FONT_AR,
                     anchor="mt", outline="#12060c")
        px.draw_text(out, en, 25 * K, y + 30, 15, "#d8d2e8", px.FONT_UI,
                     anchor="mt", outline="#12060c")

    px.draw_text(out, "مدير الملفات", (fx0 + fx1) // 2 * K, (fy0 + 2) * K, 20,
                 "warm.screen_dim", px.FONT_AR, anchor="mt")
    px.draw_text(out, "مفكرة", (nx0 + nx1) // 2 * K, (ny0 + 2) * K, 20,
                 "warm.screen_lit", px.FONT_AR, anchor="mt",
                 shadow=(1, 1, "warm.bezel_dark"))

    for i, (_icon, ar, _en) in enumerate(ui.SIDEBAR_PLACES):
        ry = fy0 + ui.TITLE_H + 15 + i * ui.SIDEBAR_ROW
        if ry + ui.SIDEBAR_ROW > fy1:
            break
        px.draw_text(out, ar, (fx0 + 15) * K, ry * K + 6, 17, "warm.screen_lit",
                     px.FONT_AR)

    for ar, en in FOLDERS:
        i = FOLDERS.index((ar, en))
        col, row = i % 3, i // 3
        lx = fx0 + 66 + col * 40 + 11
        ly = fy0 + ui.TITLE_H + 20 + row * 48 + 30
        # A label obeys the stacking order its icon already obeys.
        if lx + 14 >= nx0 and lx - 14 <= nx1 and ly + 10 >= ny0 and ly - 4 <= ny1:
            continue
        px.draw_text(out, ar, lx * K, ly * K, 17, "warm.ink", px.FONT_AR, anchor="mt")
        px.draw_text(out, en, lx * K, ly * K + 22, 13, "warm.ink_soft", px.FONT_UI,
                     anchor="mt")

    notes = [
        ("ar", "نظام بُني من الصفر."),
        ("ar", "الثيم في أصل النظام، لا فوقه."),
        ("gap", ""),
        ("en", "> hajimectl status"),
        ("ok", "> everything up · 512 MB free"),
        ("en", "> hajimectl snapshot before-upgrade"),
        ("ok", "> hajime-before-upgrade-20260806"),
    ]
    ty = (ny0 + ui.TITLE_H + 20) * K
    for kind, line in notes:
        if kind == "ar":
            px.draw_text(out, line, (nx1 - 8) * K, ty, 19, "warm.ink", px.FONT_AR,
                         anchor="rt")
            ty += 28
        elif kind == "gap":
            ty += 14
        else:
            px.draw_text(out, line, (nx0 + 8) * K, ty, 17,
                         "warm.good" if kind == "ok" else "warm.ink", px.FONT_TERM,
                         bold=True)
            ty += 24

    px.draw_text(out, "ابدأ", 26 * K, (py + 4) * K, 20, "warm.ink", px.FONT_AR,
                 anchor="mt")
    px.draw_text(out, "9:41", (bw - 26) * K, (py + 5) * K, 17, "warm.screen_lit",
                 px.FONT_UI_BOLD, anchor="mt")

    out = city.bloom(out, strength=0.22)
    # A light hand. The tube is meant to be the room the picture is in, not a
    # filter over the top of it -- and every one of these numbers costs contrast
    # in the text.
    return px.crt(out, scan_period=3, scan_strength=0.05, bloom=2,
                  bloom_strength=0.05, vig=0.16)
