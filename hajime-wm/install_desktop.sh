#!/bin/sh
# =============================================================================
# Hajime desktop installer for FreeBSD 14.4.
#
# Installs wayfire and its supporting pieces, then applies the Hajime theme.
# Idempotent: running it twice changes nothing the second time.
#
# It refuses rather than guesses. A missing firmware package, a GPU outside the
# supported range or an unexpected release each stop the run with an
# explanation, because a half-configured desktop is harder to debug than one
# that never started.
#
# Usage:  sh install_desktop.sh [--dry-run]
# Exit:   0 done, 1 refused, 2 must be run as root
# =============================================================================

set -u

DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

USER_NAME="${SUDO_USER:-${DESKTOP_USER:-hajime}}"
HERE=$(dirname "$0")

say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
ok()   { printf '   ok    %s\n' "$*"; }
warn() { printf '   warn  %s\n' "$*"; }
die()  { printf '\n   REFUSED: %s\n' "$*"; exit 1; }

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '   would  %s\n' "$*"
    else
        "$@" >/dev/null 2>&1 || return 1
    fi
    return 0
}

[ "$(id -u)" -eq 0 ] || { echo "run as root: packages and rc.conf are changed"; exit 2; }

say "Hajime desktop installer"
say "target user: ${USER_NAME}"
[ "$DRY" -eq 1 ] && say "dry run: nothing will be changed"

# --- 1. the platform -------------------------------------------------------
step "platform"
REL=$(freebsd-version -r 2>/dev/null || uname -r)
say "   FreeBSD ${REL}"
case "$REL" in
    14.4*|15.*) ok "a supported release" ;;
    14.0*|14.1*|14.2*|14.3*)
        die "FreeBSD ${REL} is end-of-life and receives no security patches.
            Upgrade to 14.4 before installing a desktop on a machine that
            hosts public sites." ;;
    *) warn "unrecognised release; continuing, but the package matrix assumes 14.4" ;;
esac

# --- 2. the GPU ------------------------------------------------------------
step "graphics hardware"
GPU=$(pciconf -lv 2>/dev/null | grep -B3 -i 'display' | grep -i "device.*=" | head -1 |
      sed "s/.*= '//;s/'.*//")
if [ -n "$GPU" ]; then
    ok "${GPU}"
else
    die "no display controller found. This machine has no GPU to drive."
fi

# The firmware blobs are named by Intel codename, not marketing name: a
# Kaby Lake HD 630 needs the `kabylake` package, and so does a Coffee Lake
# UHD 630, because both are Gen9.5 and share i915/kbl_dmc_ver1_04.bin.
FIRMWARE="gpu-firmware-intel-kabylake"
if pciconf -lv 2>/dev/null | grep -qi 'kabylake\|HD Graphics 6[0-9][0-9]\|UHD Graphics 6'; then
    ok "Gen9.5 Intel graphics: ${FIRMWARE} is the right firmware"
else
    warn "not recognised as Gen9.5; verify the firmware package for this chip"
fi

# --- 3. packages -----------------------------------------------------------
step "packages"
# drm-kmod is the meta-port: it selects the driver version matching this
# kernel. Pinning a specific one by hand is what produces the version
# mismatches people hit on point releases.
# wayfire and wf-shell are the desktop. The five after them are what the
# launchers in wf-shell.ini actually start: without them the panel has buttons
# that do nothing, which is worse than a panel with fewer buttons.
#
# badwolf is the browser xdg-open hands the console URL to. netsurf and dillo
# are lighter still but ship no working JavaScript engine, and the console
# page needs one -- a browser that cannot run it is not light, it is useless.
# falkon and qutebrowser run, but both pull in all of Qt WebEngine, which is a
# second Chromium-class engine on a machine with 8 GB of RAM that is also
# running nineteen services. badwolf is WebKitGTK with almost no shell around
# it: a real engine without the second-Chromium cost.
#
# imv is for the splash and is the one thing here that is optional in practice:
# wayfire.ini guards on it, so a session still starts if it is missing.
PKGS="drm-kmod ${FIRMWARE} seatd wayfire wf-shell xterm thunar imv xdg-utils badwolf"

for p in $PKGS; do
    if pkg info -e "$p" 2>/dev/null; then
        ok "$p already installed"
    else
        printf '   install %s ... ' "$p"
        if run pkg install -y "$p"; then
            printf 'done\n'
        else
            printf 'FAILED\n'
            die "could not install ${p}. Nothing further was changed."
        fi
    fi
done

# --- 4. kernel module and seat -------------------------------------------
step "kernel and seat"
CURRENT_KLD=$(sysrc -n kld_list 2>/dev/null || echo "")
case " ${CURRENT_KLD} " in
    *" i915kms "*) ok "i915kms already in kld_list" ;;
    *)
        run sysrc kld_list="${CURRENT_KLD} i915kms" && ok "added i915kms to kld_list"
        ;;
esac
run sysrc seatd_enable="YES" && ok "seatd enabled"
if [ "$DRY" -eq 0 ] && ! service seatd onestatus >/dev/null 2>&1; then
    run service seatd start && ok "seatd started"
fi

# The user needs to be in the video group to reach the GPU.
if pw groupshow video 2>/dev/null | grep -q "\b${USER_NAME}\b"; then
    ok "${USER_NAME} already in the video group"
else
    run pw groupmod video -m "${USER_NAME}" && ok "added ${USER_NAME} to video"
fi

# --- 5. the theme ----------------------------------------------------------
step "theme"
HOME_DIR=$(getent passwd "${USER_NAME}" 2>/dev/null | cut -d: -f6)
HOME_DIR="${HOME_DIR:-/home/${USER_NAME}}"

# The stylesheet and the palette it imports. Installing one without the other
# leaves GTK resolving @screen to nothing and drawing every window transparent.
PALETTE="${HERE}/../hajime-brand/out/palette-gtk.css"
[ -f "$PALETTE" ] || die "missing ${PALETTE}
            The palette is generated: python hajime-brand/tools/emit.py"

for target in "${HOME_DIR}/.config/gtk-3.0" "${HOME_DIR}/.config/gtk-4.0"; do
    run install -d -o "${USER_NAME}" -m 755 "$target"
    if run install -o "${USER_NAME}" -m 644 "${HERE}/hajime_theme.css" "${target}/gtk.css" &&
       run install -o "${USER_NAME}" -m 644 "$PALETTE" "${target}/palette-gtk.css"; then
        ok "theme and palette installed to ${target}"
    else
        warn "could not install the theme to ${target}"
    fi
done

if run install -o "${USER_NAME}" -m 644 "${HERE}/wayfire.ini" "${HOME_DIR}/.config/wayfire.ini"; then
    ok "wayfire.ini installed"
else
    warn "could not install wayfire.ini"
fi

# wf-panel and wf-background read their own file. The wallpaper path lives
# there, and putting it in wayfire.ini instead fails without saying anything.
if run install -o "${USER_NAME}" -m 644 "${HERE}/wf-shell.ini" "${HOME_DIR}/.config/wf-shell.ini"; then
    ok "wf-shell.ini installed (panel at the bottom, wallpaper, launchers)"
else
    warn "could not install wf-shell.ini"
fi

# There is no GNOME or KDE session here for xdg-open to detect, so it falls
# through to `xdg-mime query default x-scheme-handler/http`, which reads
# $XDG_CONFIG_HOME/mimeapps.list -- $HOME/.config/mimeapps.list, since nothing
# on this desktop sets XDG_CONFIG_HOME. Without this file the console
# launcher's `xdg-open http://127.0.0.1:8088/` finds no handler and the button
# does nothing. The name it points at, badwolf.desktop, is not written by this
# repo: the www/badwolf package installs it to
# /usr/local/share/applications/badwolf.desktop, which is on the default
# XDG_DATA_DIRS search path and is where xdg-mime resolves the name from.
if run install -o "${USER_NAME}" -m 644 "${HERE}/mimeapps.list" "${HOME_DIR}/.config/mimeapps.list"; then
    ok "mimeapps.list installed (badwolf is the default browser)"
else
    warn "could not install mimeapps.list"
fi

# --- 6. verdict ------------------------------------------------------------
step "next"
if [ "$DRY" -eq 1 ]; then
    say "   dry run complete; nothing was changed"
    exit 0
fi

say "   Reboot so i915kms loads from kld_list, then as ${USER_NAME}:"
say ""
say "     wayfire"
say ""
say "   If the screen stays black, the firmware is the first thing to check:"
say ""
say "     dmesg | grep -i drm | grep -i firmware"
say ""
say "   A line reading 'successfully loaded firmware image' means the GPU is"
say "   working and the problem is elsewhere. A 'not found' line names the"
say "   file, and its prefix names the package."
say ""
say "   The diagnostic script covers the rest:  sh wayland_feasibility_test.sh"
say ""
say "   The wallpaper, the splash and the launcher icons come from the theme"
say "   installer, which also sets the loader screen and the console palette:"
say ""
say "     sh hajime-brand/install_theme.sh"
say ""
say "   And the display language, which is a login class like on any other"
say "   system rather than a translation painted into the pictures:"
say ""
say "     hajime-lang ar        (or: hajime-lang en)"
exit 0
