#!/bin/sh
# =============================================================================
# Hajime theme installer for FreeBSD 14.4.
#
# Puts the theme where the system reads it from, rather than on top of a system
# that has already decided what it looks like:
#
#   the loader        a logo and a brand it draws before the kernel starts
#   the kernel console  sixteen palette entries and a 16x32 font, so every
#                       message from device probe to panic is in these colours
#   the login          a message of the day carrying the credits and the ask
#   the boot's end     an rc script that prints the mascot and four real checks
#   the desktop        wallpaper, splash, icons and the GTK palette
#
# Everything it writes into a file it already found is written inside a marked
# block, so running it twice replaces rather than stacks. Everything it replaces
# outright is copied to <file>.hajime-orig first, once, and never overwritten
# after that -- the first copy is the one from before this system touched it.
#
# Usage:  sh install_theme.sh [--dry-run] [--no-loader] [--no-desktop]
# Exit:   0 done, 1 refused, 2 not root
#
# --no-loader is for a machine that boots from somewhere this script should not
# touch: a jail, a rescue environment, or a host whose /boot is managed
# elsewhere. Everything above the loader still gets installed.
# =============================================================================

set -u

DRY=0
DO_LOADER=1
DO_DESKTOP=1

for arg in "$@"; do
    case "$arg" in
        --dry-run)    DRY=1 ;;
        --no-loader)  DO_LOADER=0 ;;
        --no-desktop) DO_DESKTOP=0 ;;
        -h|--help)    sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 1 ;;
    esac
done

HERE=$(cd "$(dirname "$0")" && pwd)
OUT="${HERE}/out"
SHARE=/usr/local/share/hajime
RCD=/usr/local/etc/rc.d
BEGIN="# --- BEGIN hajime theme ---"
END="# --- END hajime theme ---"

WARNINGS=0
say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
ok()   { printf '   ok    %s\n' "$*"; }
warn() { printf '   warn  %s\n' "$*"; WARNINGS=$((WARNINGS + 1)); }
# A real run stops at the first blocker. A dry run notes it and keeps going: the
# point of a dry run is to see the whole plan, and one that halts on the first
# problem hides the four behind it. Same rule as install_hajime_os.sh, which
# this script did not follow until it was pointed out.
BLOCKERS=0
die() {
    if [ "$DRY" -eq 1 ]; then
        printf '   WOULD REFUSE: %s\n' "$*"
        BLOCKERS=$((BLOCKERS + 1))
        return 0
    fi
    printf '\n   REFUSED: %s\n' "$*" >&2
    exit 1
}

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '   would  %s\n' "$*"
        return 0
    fi
    "$@" >/dev/null 2>&1
}

# One backup, taken the first time and never again. A second copy taken on the
# second run would preserve this script's own output as "the original".
keep_original() {
    file="$1"
    [ -f "$file" ] || return 0
    [ -f "${file}.hajime-orig" ] && return 0
    if [ "$DRY" -eq 1 ]; then
        printf '   would  copy %s to %s.hajime-orig\n' "$file" "$file"
        return 0
    fi
    cp -p "$file" "${file}.hajime-orig"
}

# Replace the marked block in a file, or append one if it is not there yet.
write_block() {
    file="$1"; body="$2"
    if [ "$DRY" -eq 1 ]; then
        printf '   would  rewrite the hajime block in %s\n' "$file"
        return 0
    fi
    keep_original "$file"
    touch "$file" || return 1
    tmp="${file}.hajime.$$"
    awk -v b="$BEGIN" -v e="$END" '
        $0 == b { skip = 1 }
        !skip   { print }
        $0 == e { skip = 0; next }
    ' "$file" > "$tmp" || return 1
    printf '%s\n%s%s\n' "$BEGIN" "$body" "$END" >> "$tmp" || return 1
    mv "$tmp" "$file"
}

put() {
    src="$1"; dest="$2"; mode="${3:-644}"
    # The `return` matters now that die comes back in a dry run: without it the
    # next line would print "would install" for a file that is not there.
    [ -f "$src" ] || { die "missing generated file: ${src}
            Run: python hajime-brand/tools/emit.py"; return 1; }
    keep_original "$dest"
    run install -m "$mode" "$src" "$dest" || return 1
    return 0
}

[ "$(id -u)" -eq 0 ] || { echo "run as root: this writes to /boot and /etc" >&2; exit 2; }

say "Hajime theme installer"
[ "$DRY" -eq 1 ] && say "dry run: nothing will be changed"

# --- 1. the platform -------------------------------------------------------
step "platform"
SYS=$(uname -s)
[ "$SYS" = "FreeBSD" ] || die "this installs FreeBSD loader and vt(4) settings; this is ${SYS}."
REL=$(freebsd-version -r 2>/dev/null || uname -r)
case "$REL" in
    14.*|15.*) ok "FreeBSD ${REL}" ;;
    *) warn "FreeBSD ${REL}: the loader graphics format below is the 14.x one" ;;
esac

[ -d "$OUT" ] || die "no ${OUT}. The theme is generated, not stored:
            python hajime-brand/tools/emit.py"
ok "generated assets found in ${OUT}"

# --- 2. shared assets ------------------------------------------------------
step "assets"
run install -d -m 755 "$SHARE" || die "could not create ${SHARE}"
for f in wallpaper-1920x1080.png splash-1920x1080.png boot-1920x1080.png \
         console-logo.ansi console-credits.ansi mark-256.png mark-64.png \
         mark-32.png mark.svg; do
    if put "${OUT}/${f}" "${SHARE}/${f}"; then
        ok "${SHARE}/${f}"
    else
        warn "could not install ${f}"
    fi
done
# The wallpaper under a stable name, so wayfire.ini does not carry a resolution.
run ln -sf "${SHARE}/wallpaper-1920x1080.png" "${SHARE}/wallpaper.png" \
    && ok "${SHARE}/wallpaper.png -> wallpaper-1920x1080.png"

# --- 3. the loader ---------------------------------------------------------
if [ "$DO_LOADER" -eq 1 ]; then
    step "loader"
    if [ ! -d /boot/lua ]; then
        warn "/boot/lua is missing; this machine boots something other than the Lua loader"
    else
        put "${OUT}/gfx-hajime.lua" /boot/lua/gfx-hajime.lua && ok "/boot/lua/gfx-hajime.lua"
        run install -d -m 755 /boot/images
        put "${OUT}/hajime-logo.png" /boot/images/hajime-logo.png && ok "/boot/images/hajime-logo.png"

        # Parse it before the loader has to. A syntax error here is a boot that
        # stops at a Lua traceback, on a machine with no IPMI.
        if [ "$DRY" -eq 0 ] && [ -x /usr/libexec/flua ]; then
            if /usr/libexec/flua -e 'assert(loadfile("/boot/lua/gfx-hajime.lua"))' 2>/dev/null; then
                ok "gfx-hajime.lua parses"
            else
                die "gfx-hajime.lua does not parse. /boot/loader.conf was left alone,
            so the machine still boots with the stock logo."
            fi
        fi

        LOADER_BODY=$(cat "${OUT}/loader.conf.vt")
        if write_block /boot/loader.conf "$LOADER_BODY"; then
            ok "loader.conf: console palette, 16x32 font, logo and brand"
        else
            die "could not write /boot/loader.conf"
        fi
    fi
else
    say "   loader skipped by request"
fi

# --- 4. the message of the day ---------------------------------------------
# The credits and the ask live here rather than in the pre-login banner. That
# banner is a capability record in /etc/gettytab, where a stray colon costs you
# the console on a machine with no other way in, and the words are the same one
# keystroke later.
step "login"
if [ -f "${OUT}/motd.hajime" ]; then
    MOTD_BODY=$(cat "${OUT}/motd.hajime")
    if write_block /etc/motd.template "$MOTD_BODY"; then
        ok "/etc/motd.template"
    else
        warn "could not write /etc/motd.template"
    fi
else
    warn "no motd.hajime; run tools/emit.py"
fi

# --- 5. the boot splash service --------------------------------------------
step "boot splash"
# /usr/local/etc/rc.d does not exist on a FreeBSD that has never installed a
# package, and `install` will not create a missing parent. On the machine this
# was written for it was always there; on a first install it never is, and the
# splash service silently failed to install while everything around it worked.
run install -d -m 755 "$RCD"
if put "${HERE}/rc.d/hajime_splash" "${RCD}/hajime_splash" 755; then
    ok "${RCD}/hajime_splash"
    run sysrc hajime_splash_enable="YES" >/dev/null && ok "enabled at boot"
else
    warn "could not install the splash service"
fi

# --- 6. the typefaces ------------------------------------------------------
# The same three files the artwork was drawn with. Naming a font in a stylesheet
# and hoping the machine has it is how a themed desktop ends up in DejaVu Sans.
step "fonts"
FONTDIR=/usr/local/share/fonts/hajime
run install -d -m 755 "$FONTDIR"
for f in PressStart2P-Regular.ttf VT323-Regular.ttf Almarai-Regular.ttf \
         Almarai-Bold.ttf OFL-PressStart2P.txt OFL-VT323.txt OFL-Almarai.txt; do
    if [ -f "${HERE}/fonts/${f}" ]; then
        put "${HERE}/fonts/${f}" "${FONTDIR}/${f}" && ok "${f}"
    else
        warn "missing font file ${f}"
    fi
done
if command -v fc-cache >/dev/null 2>&1; then
    run fc-cache -f "$FONTDIR" && ok "font cache rebuilt"
else
    warn "fc-cache not found; the fonts are installed but not yet indexed.
            They will be picked up when fontconfig is installed with the desktop."
fi

# --- 7. the language -------------------------------------------------------
# Arabic is a login class, not a picture. Both classes are added; which one a
# user is in is what `hajime-lang` changes.
#
# /etc/login.conf is a capability database and a stray colon in it costs every
# login on a machine with no second way in. So: back it up, write, rebuild, and
# put the backup straight back if the rebuild complains.
step "language"
if [ -f /etc/login.conf ]; then
    LOGIN_BODY=$(cat "${HERE}/login.conf.hajime")
    if write_block /etc/login.conf "$LOGIN_BODY"; then
        if [ "$DRY" -eq 1 ]; then
            ok "would add the hajime-ar and hajime-en classes"
        elif cap_mkdb /etc/login.conf 2>/dev/null; then
            ok "hajime-ar and hajime-en classes added, database rebuilt"
        else
            cp -p /etc/login.conf.hajime-orig /etc/login.conf 2>/dev/null
            cap_mkdb /etc/login.conf 2>/dev/null
            die "cap_mkdb rejected the new /etc/login.conf. The original was put
            back and the database rebuilt from it, so logins still work."
        fi
    else
        warn "could not write /etc/login.conf"
    fi
else
    warn "/etc/login.conf is missing; the language classes were not added"
fi

run install -d -m 755 /usr/local/bin   # same reason as rc.d above
if put "${HERE}/bin/hajime-lang" /usr/local/bin/hajime-lang 755; then
    ok "/usr/local/bin/hajime-lang"
    say "   set a user's language with:  hajime-lang ar    (or: hajime-lang en)"
fi

# --- 8. the desktop --------------------------------------------------------
if [ "$DO_DESKTOP" -eq 1 ]; then
    step "desktop"
    USER_NAME="${SUDO_USER:-${DESKTOP_USER:-hajime}}"
    HOME_DIR=$(getent passwd "${USER_NAME}" 2>/dev/null | cut -d: -f6)
    HOME_DIR="${HOME_DIR:-/home/${USER_NAME}}"

    if [ ! -d "$HOME_DIR" ]; then
        warn "no home for ${USER_NAME}; the GTK palette was not installed.
            The desktop installer (hajime-wm/install_desktop.sh) does this part."
    else
        for target in "${HOME_DIR}/.config/gtk-3.0" "${HOME_DIR}/.config/gtk-4.0"; do
            run install -d -o "${USER_NAME}" -m 755 "$target"
            put "${OUT}/palette-gtk.css" "${target}/palette-gtk.css" && \
                ok "${target}/palette-gtk.css"
        done
    fi

    # Launchers, with their Arabic names inside them. This is where a desktop
    # keeps a translation: one file per application, one Name[xx] line per
    # language, resolved by the locale at run time.
    run install -d -m 755 /usr/local/share/applications
    for f in "${HERE}"/desktop/*.desktop; do
        [ -f "$f" ] || continue
        put "$f" "/usr/local/share/applications/$(basename "$f")" && \
            ok "$(basename "$f")"
    done
else
    say "   desktop skipped by request"
fi

# --- 9. verdict ------------------------------------------------------------
step "next"
if [ "$DRY" -eq 1 ]; then
    say "   Dry run complete; nothing was changed."
    say "   ${WARNINGS} warning(s), ${BLOCKERS} blocker(s)."
    if [ "$BLOCKERS" -gt 0 ]; then
        say ""
        say "   A real run would stop at the first WOULD REFUSE above."
        exit 1
    fi
    say "   A real run would proceed."
    exit 0
fi

say "   Installed, ${WARNINGS} warning(s)."
say ""
say "   The loader's screen and the console palette appear on the next boot."
say "   Nothing else needs a reboot."
say ""
say "   To ask the machine what it actually has, now and after the reboot:"
say ""
say "     sh hajime-brand/verify_theme.sh"
say ""
say "   It separates what is written into a file from what the running kernel"
say "   is using, which is the difference between a themed config and a themed"
say "   system. Before the reboot most of it reads 'written'."
say ""
say "   To see the console art without rebooting:"
say "     service hajime_splash onestart"
say ""
say "   To undo: every file this touched has a .hajime-orig beside it, and the"
say "   blocks in loader.conf and motd.template are between the markers."
exit 0
