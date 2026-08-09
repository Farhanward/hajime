#!/bin/sh
# =============================================================================
# Does every file the theme refers to actually exist?
#
# The installer reads about thirty paths and writes them somewhere else. Most of
# those paths are generated, and a generated file that is missing does not fail
# at the moment it goes missing -- it fails halfway through an install on a
# machine with no console, after /boot/loader.conf has already been rewritten.
#
# This walks the references in the other direction: for each thing the installer
# or a config file names, does the source exist here, now, before anyone runs
# anything. It needs no root and no FreeBSD, so it runs on the machine the
# theme is edited on and in CI.
#
# Usage:  sh check_wiring.sh
# Exit:   0 everything referenced is present, 1 something is missing
# =============================================================================

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "${HERE}/.." && pwd)
MISSING=0
CHECKED=0

ok()   { printf '   ok    %s\n' "$*"; CHECKED=$((CHECKED + 1)); }
bad()  { printf '   MISSING  %s\n' "$*"; MISSING=$((MISSING + 1)); CHECKED=$((CHECKED + 1)); }
step() { printf '\n== %s\n' "$*"; }

want() {
    if [ -e "$1" ]; then
        ok "${1#"${ROOT}"/}"
    else
        bad "${1#"${ROOT}"/}   ($2)"
    fi
}

echo "Hajime theme wiring check"

# --- what install_theme.sh copies -----------------------------------------
step "generated assets"
for f in gfx-hajime.lua hajime-logo.png loader.conf.vt motd.hajime \
         console-logo.ansi console-credits.ansi mark.svg mark-32.png \
         mark-64.png mark-256.png palette.css palette-gtk.css brand.rs \
         wayfire.colors wallpaper-1920x1080.png splash-1920x1080.png \
         boot-1920x1080.png; do
    want "${HERE}/out/${f}" "install_theme.sh"
done

step "carried files"
want "${HERE}/rc.d/hajime_splash" "the boot splash service"
want "${HERE}/bin/hajime-lang" "the language switch"
want "${HERE}/login.conf.hajime" "the two login classes"
want "${HERE}/palette.toml" "the palette everything is printed from"
want "${HERE}/brand.toml" "the identity everything is printed from"
for f in PressStart2P-Regular.ttf VT323-Regular.ttf Almarai-Regular.ttf \
         Almarai-Bold.ttf; do
    want "${HERE}/fonts/${f}" "installed into /usr/local/share/fonts/hajime"
done
for f in OFL-PressStart2P.txt OFL-VT323.txt OFL-Almarai.txt; do
    want "${HERE}/fonts/${f}" "the licence that has to travel with the font"
done

# --- the launchers, and the applications they name ------------------------
step "launchers"
for f in "${HERE}"/desktop/*.desktop; do
    [ -e "$f" ] || { bad "hajime-brand/desktop/*.desktop  (none found)"; break; }
    name=$(basename "$f")
    # Both names, or the panel is in one language whatever the session is.
    if grep -q '^Name=' "$f" && grep -q '^Name\[ar\]=' "$f"; then
        ok "${name}  (Name and Name[ar])"
    else
        bad "${name}  (a launcher without both names)"
    fi
done

# Every launcher wf-shell.ini pins must be a file that exists.
step "panel launchers resolve"
SHELL_INI="${ROOT}/hajime-wm/wf-shell.ini"
want "$SHELL_INI" "the panel and background config"
if [ -f "$SHELL_INI" ]; then
    ENTRIES=$(sed -n 's/^launcher_[a-z]*[[:space:]]*=[[:space:]]*//p' "$SHELL_INI")
    # Word splitting on purpose, and not piped into `while read`: a pipeline
    # runs the loop in a subshell, so a missing launcher printed MISSING and
    # left the exit code at zero. Desktop entry names have no spaces in them.
    # shellcheck disable=SC2086
    for entry in $ENTRIES; do
        want "${HERE}/desktop/${entry}" "pinned in wf-shell.ini"
    done
fi

# --- what the console compiles in -----------------------------------------
step "console includes"
CONSOLE="${ROOT}/hajime-console/src"
for pair in "lib.rs:out/brand.rs" "main.rs:out/palette.css" "main.rs:out/mark.svg" \
            "render.rs:out/mark.svg"; do
    src="${CONSOLE}/${pair%%:*}"
    target="${HERE}/${pair#*:}"
    if [ ! -f "$src" ]; then
        bad "hajime-console/src/${pair%%:*}"
    elif grep -q "hajime-brand/${pair#*:}" "$src"; then
        want "$target" "included by ${pair%%:*}"
    else
        bad "hajime-console/src/${pair%%:*} no longer includes ${pair#*:}"
    fi
done

# --- the stylesheet's import ----------------------------------------------
step "gtk theme"
THEME="${ROOT}/hajime-wm/hajime_theme.css"
want "$THEME" "the GTK stylesheet"
if [ -f "$THEME" ]; then
    if grep -q 'palette-gtk.css' "$THEME"; then
        ok "hajime_theme.css imports palette-gtk.css"
    else
        bad "hajime_theme.css does not import the generated palette"
    fi
fi

# --- is anything installed that nobody reads? -----------------------------
# The installer copies these into /usr/local/share/hajime. Something has to name
# each of them afterwards, or it is dead weight sitting on the target.
#
# Matched by file name rather than by full path. The first version searched for
# the absolute path and reported two live references as broken, because the rc
# script builds its paths from ${hajime_splash_share} and never writes the
# directory out. The check was wrong, not the script.
step "nothing is installed unread"
for f in wallpaper.png splash-1920x1080.png console-logo.ansi \
         console-credits.ansi mark-32.png; do
    users=$(grep -rl -- "$f" "${ROOT}/hajime-wm" "${HERE}/rc.d" "${HERE}/bin" \
            "${HERE}/desktop" 2>/dev/null | grep -c .)
    if [ "$users" -gt 0 ]; then
        ok "${f}  (read by ${users} file(s))"
    else
        bad "${f}  (installed, and nothing reads it)"
    fi
done

# --- verdict ---------------------------------------------------------------
printf '\n'
if [ "$MISSING" -gt 0 ]; then
    printf '%d of %d references are broken.\n' "$MISSING" "$CHECKED"
    printf 'If the generated files are the ones missing:  python hajime-brand/tools/emit.py\n'
    exit 1
fi
printf 'all %d references resolve.\n' "$CHECKED"
exit 0
