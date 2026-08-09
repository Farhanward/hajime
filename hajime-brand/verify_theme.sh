#!/bin/sh
# =============================================================================
# Is the theme actually in this system, or only in its config files?
#
# The difference is the whole point of this script. A line in /boot/loader.conf
# saying the console palette is the system's colours proves that somebody wrote
# it there. What proves the kernel is using it is `kenv`, which reads the
# environment the loader handed the kernel before userland existed. One is a
# claim, the other is the machine answering.
#
# So every check reports one of three states:
#
#   live      the running system has it now
#   written   it is in the file and will apply at the next boot
#   missing   it is not there at all
#
# `written` is not a failure. It is what everything looks like between running
# the installer and rebooting, and saying so is more use than a green tick that
# means "the file parses".
#
# Needs no root: everything here is read.
#
# Usage:  sh verify_theme.sh [--user <name>]
# Exit:   0 nothing missing, 1 something is missing
# =============================================================================

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
OUT="${HERE}/out"
SHARE=/usr/local/share/hajime
BEGIN="# --- BEGIN hajime theme ---"

DESK_USER="${SUDO_USER:-${USER:-hajime}}"
[ "${1:-}" = "--user" ] && { DESK_USER="${2:-$DESK_USER}"; shift 2; }

MISSING=0
PENDING=0

row() { printf '   %-9s %-34s %s\n' "$1" "$2" "${3:-}"; }
live()    { row "live" "$1" "${2:-}"; }
written() { row "written" "$1" "${2:-}"; PENDING=$((PENDING + 1)); }
gone()    { row "MISSING" "$1" "${2:-}"; MISSING=$((MISSING + 1)); }
step()    { printf '\n== %s\n' "$*"; }

# kenv exits non-zero for a name it does not have, which is exactly the test.
kenv_get() { kenv "$1" 2>/dev/null; }

if [ "$(uname -s)" != "FreeBSD" ]; then
    echo "This reads FreeBSD's kernel environment and rc configuration."
    echo "On $(uname -s) there is nothing here to look at."
    exit 1
fi

echo "Hajime theme: what is in this system"
echo "host: $(hostname)   $(freebsd-version -r 2>/dev/null || uname -r)"

# --- the loader ------------------------------------------------------------
step "the loader's screen"

if [ -f /boot/lua/gfx-hajime.lua ]; then
    if [ -x /usr/libexec/flua ] &&
       /usr/libexec/flua -e 'assert(loadfile("/boot/lua/gfx-hajime.lua"))' 2>/dev/null; then
        detail="parses"
    else
        detail="present"
    fi
    # loader_logo in the kernel environment means the loader read the name and
    # went looking for this file. It is the closest thing to a receipt.
    if [ "$(kenv_get loader_logo)" = "hajime" ]; then
        live "/boot/lua/gfx-hajime.lua" "$detail, and the loader asked for it"
    else
        written "/boot/lua/gfx-hajime.lua" "$detail, not selected at the last boot"
    fi
else
    gone "/boot/lua/gfx-hajime.lua" "run install_theme.sh"
fi

if [ -f /boot/images/hajime-logo.png ]; then
    live "/boot/images/hajime-logo.png"         "$(stat -f %z /boot/images/hajime-logo.png 2>/dev/null) bytes"
else
    gone "/boot/images/hajime-logo.png"
fi

if [ "$(kenv_get loader_brand)" = "hajime" ]; then
    live "loader_brand" "hajime"
else
    written "loader_brand" "set in loader.conf, not in this boot's environment"
fi

# --- the kernel console ----------------------------------------------------
step "the kernel console"

# Compare what the generator says the palette is against what the kernel was
# handed. This is the check that means the theme is in the system rather than
# on it: nothing in userland can put a colour here.
if [ ! -f "${OUT}/loader.conf.vt" ]; then
    gone "the generated palette" "python hajime-brand/tools/emit.py"
else
    agree=0; differ=0; absent=0
    for slot in 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
        want=$(sed -n "s/^kern\.vt\.color\.${slot}\.rgb=\"\([^\"]*\)\".*/\1/p" \
               "${OUT}/loader.conf.vt")
        [ -n "$want" ] || continue
        got=$(kenv_get "kern.vt.color.${slot}.rgb")
        if [ -z "$got" ]; then
            absent=$((absent + 1))
        elif [ "$got" = "$want" ]; then
            agree=$((agree + 1))
        else
            differ=$((differ + 1))
        fi
    done
    if [ "$agree" -gt 0 ] && [ "$differ" -eq 0 ] && [ "$absent" -eq 0 ]; then
        live "kern.vt.color.0-15" "all 16 match palette.toml"
    elif [ "$differ" -gt 0 ]; then
        gone "kern.vt.color.0-15" "${differ} slot(s) differ from palette.toml"
    elif grep -q "^kern.vt.color" /boot/loader.conf 2>/dev/null; then
        written "kern.vt.color.0-15" "in loader.conf, applies at the next boot"
    else
        gone "kern.vt.color.0-15" "not in loader.conf"
    fi
fi

font=$(kenv_get screen.font)
if [ -n "$font" ]; then
    live "screen.font" "$font"
elif grep -q '^screen.font' /boot/loader.conf 2>/dev/null; then
    written "screen.font" "applies at the next boot"
else
    gone "screen.font"
fi

if grep -q -- "$BEGIN" /boot/loader.conf 2>/dev/null; then
    live "/boot/loader.conf" "the hajime block is present"
else
    gone "/boot/loader.conf" "no hajime block"
fi

# --- the end of the boot, and the login ------------------------------------
step "boot and login"

if [ -f /usr/local/etc/rc.d/hajime_splash ]; then
    case "$(sysrc -n hajime_splash_enable 2>/dev/null)" in
        [Yy][Ee][Ss]) live "hajime_splash" "enabled" ;;
        *) written "hajime_splash" "installed, not enabled: sysrc hajime_splash_enable=YES" ;;
    esac
else
    gone "hajime_splash" "run install_theme.sh"
fi

if grep -q -- "$BEGIN" /etc/motd.template 2>/dev/null; then
    # /etc/motd is regenerated from the template at boot, so finding the
    # repository line in the generated file means the template has been through
    # a boot rather than merely been edited.
    if [ -f "${OUT}/motd.hajime" ] &&
       grep -qF "$(sed -n '3p' "${OUT}/motd.hajime")" /etc/motd 2>/dev/null; then
        live "/etc/motd" "regenerated from the template"
    else
        written "/etc/motd.template" "applies at the next boot"
    fi
else
    gone "/etc/motd.template" "no hajime block"
fi

# --- the language ----------------------------------------------------------
step "the language"

if grep -q '^hajime-ar:' /etc/login.conf 2>/dev/null; then
    # The text is not what login reads. cap_mkdb builds a database beside it,
    # and a class edited without rebuilding is a class that does not exist.
    if [ /etc/login.conf.db -nt /etc/login.conf ]; then
        live "hajime-ar, hajime-en" "in login.conf, database is current"
    else
        gone "hajime-ar, hajime-en" "login.conf.db is older than login.conf: cap_mkdb /etc/login.conf"
    fi
else
    gone "hajime-ar, hajime-en" "no classes in /etc/login.conf"
fi

class=$(pw usershow "$DESK_USER" 2>/dev/null | cut -d: -f5)
case "$class" in
    hajime-ar) live "${DESK_USER}'s language" "Arabic (hajime-ar)" ;;
    hajime-en) live "${DESK_USER}'s language" "English (hajime-en)" ;;
    "") written "${DESK_USER}'s language" "no class set: hajime-lang ar" ;;
    *) written "${DESK_USER}'s language" "class ${class}, not one of ours" ;;
esac

if [ -x /usr/local/bin/hajime-lang ]; then
    live "hajime-lang"
else
    gone "hajime-lang"
fi

count=$(find /usr/local/share/applications -name 'hajime-*.desktop' 2>/dev/null | wc -l | tr -d ' ')
if [ "$count" -gt 0 ]; then
    live "launchers" "${count} installed, each with Name[ar]"
else
    gone "launchers" "none in /usr/local/share/applications"
fi

# --- the desktop -----------------------------------------------------------
step "the desktop"

for f in wallpaper.png splash-1920x1080.png console-logo.ansi console-credits.ansi; do
    if [ -e "${SHARE}/${f}" ]; then
        live "${SHARE}/${f}"
    else
        gone "${SHARE}/${f}"
    fi
done

if command -v fc-list >/dev/null 2>&1; then
    if fc-list 2>/dev/null | grep -q "VT323"; then
        live "fonts" "fontconfig can see them"
    elif [ -d /usr/local/share/fonts/hajime ]; then
        written "fonts" "installed, not indexed: fc-cache -f /usr/local/share/fonts/hajime"
    else
        gone "fonts"
    fi
else
    if [ -d /usr/local/share/fonts/hajime ]; then
        written "fonts" "installed; no fontconfig on this host yet"
    else
        gone "fonts"
    fi
fi

HOME_DIR=$(getent passwd "$DESK_USER" 2>/dev/null | cut -d: -f6)
HOME_DIR="${HOME_DIR:-/home/${DESK_USER}}"
if [ -f "${HOME_DIR}/.config/gtk-3.0/gtk.css" ]; then
    if [ -f "${HOME_DIR}/.config/gtk-3.0/palette-gtk.css" ]; then
        live "GTK theme for ${DESK_USER}" "stylesheet and palette"
    else
        gone "GTK theme for ${DESK_USER}" "gtk.css imports a palette that is not there"
    fi
else
    written "GTK theme for ${DESK_USER}" "run hajime-wm/install_desktop.sh"
fi

# --- verdict ---------------------------------------------------------------
printf '\n'
if [ "$MISSING" -gt 0 ]; then
    printf '%d thing(s) missing. The theme is not fully in this system.\n' "$MISSING"
    exit 1
fi
if [ "$PENDING" -gt 0 ]; then
    printf 'Nothing missing. %d thing(s) are written but not yet live:\n' "$PENDING"
    printf 'reboot, and the loader screen and the console palette come with it.\n'
    exit 0
fi
printf 'The theme is live: the loader drew it, the kernel is printing in it,\n'
printf 'and the desktop is wearing it.\n'
exit 0
