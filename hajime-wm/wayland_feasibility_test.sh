#!/bin/sh
# =============================================================================
# Hajime OS — Wayland feasibility test for FreeBSD
#
# Answers one question: will a Wayland compositor drive this machine's GPU on
# FreeBSD? Everything in the desktop plan depends on the answer, so it is worth
# an hour before it is worth a month.
#
# Run this from a FreeBSD live USB or a scratch install on the TARGET hardware.
# Do NOT run it on the production server: it loads kernel modules.
#
# A virtual machine cannot answer this. The question is whether drm-kmod binds
# to the real Intel HD 630; a VM presents a virtual GPU instead.
#
# Usage:  sh wayland_feasibility_test.sh
# Exit:   0 viable, 1 blocked, 2 must be run as root
#
# If you would rather install everything first and let the test confirm it,
# this is the known-good set for a Kaby Lake i7-7700 with HD 630. A forum
# report of the same GPU generation reached 1920x1080@60Hz with exactly this,
# and no X11 configuration at all:
#
#   pkg install drm-kmod gpu-firmware-intel-kabylake seatd wayfire wf-shell
#   sysrc kld_list="i915kms"
#   sysrc seatd_enable="YES"
#   service seatd start
#
# The firmware package is the part people miss. Without it the driver loads but
# the display never comes up, and the failure looks like a driver problem.
# =============================================================================

set -u

PASS=0
FAIL=0
REPORT="/tmp/hajime_wayland_report.txt"

say()  { printf '%s\n' "$*" | tee -a "$REPORT"; }
ok()   { PASS=$((PASS+1)); say "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); say "  FAIL  $*"; }
note() { say "        $*"; }

[ "$(id -u)" -eq 0 ] || { echo "run as root: kernel modules are loaded"; exit 2; }

: > "$REPORT"
say "Hajime OS — Wayland feasibility test"
say "date: $(date)"
say ""

# --- 1. platform -----------------------------------------------------------
say "[1] platform"
REL=$(freebsd-version -r 2>/dev/null || uname -r)
say "        FreeBSD $REL on $(uname -m)"
case "$REL" in
    14.4*|15.*)                 ok "supported release" ;;
    14.0*|14.1*|14.2*|14.3*)
        bad "FreeBSD $REL is end-of-life and gets no security patches; the \
desktop installer refuses it" ;;
    *) note "unrecognised release '$REL'; the ports matrix assumes 14.4" ;;
esac

# --- 2. the GPU ------------------------------------------------------------
say ""
say "[2] graphics hardware"
GPU=$(pciconf -lv 2>/dev/null | grep -B3 -i 'display' | grep -i 'device.*=' | head -1)
if [ -n "$GPU" ]; then
    ok "detected:$(printf '%s' "$GPU" | sed "s/.*= '//;s/'.*//")"
else
    bad "no display controller found by pciconf"
fi
pciconf -lv 2>/dev/null | grep -qi 'kabylake\|HD Graphics 6[0-9][0-9]\|UHD Graphics' \
    && ok "Kaby Lake class GPU: supported by drm-kmod" \
    || note "not recognised as Kaby Lake; verify your chip against the drm-kmod matrix"

# --- 3. drm-kmod -----------------------------------------------------------
# The known trap: packages are built against 14.1 and refuse to load on 14.2.
say ""
say "[3] drm-kmod"
if pkg info -e drm-kmod 2>/dev/null || pkg info -e drm-61-kmod 2>/dev/null; then
    ok "a drm-kmod package is installed"
else
    note "not installed. Install with:"
    note "  pkg install drm-61-kmod        # or build: cd /usr/ports/graphics/drm-kmod && make install"
    note "  If it refuses to load, the package was built for a different 14.x"
    note "  point release. Rebuild from ports against THIS kernel."
fi

# Firmware blobs are a separate package from the driver, and their absence is
# the most common cause of a GPU that "should work" but does not. The blobs are
# named by Intel codename, not by marketing name: a Kaby Lake i7-7700 with
# HD 630 needs the `kabylake` package. Coffee Lake UHD 630 needs it too, since
# both are Gen9.5 and share `i915/kbl_dmc_ver1_04.bin`.
if pkg info -e gpu-firmware-intel-kabylake 2>/dev/null; then
    ok "gpu-firmware-intel-kabylake installed"
elif pkg info 2>/dev/null | grep -q '^gpu-firmware-intel'; then
    note "a different Intel firmware package is installed:"
    pkg info 2>/dev/null | grep '^gpu-firmware-intel' | while read -r l; do note "  $l"; done
    note "Kaby Lake needs: pkg install gpu-firmware-intel-kabylake"
else
    bad "no Intel GPU firmware package installed"
    note "this alone will stop the GPU from initialising. Install it:"
    note "  pkg install gpu-firmware-intel-kabylake"
fi

if kldstat -q -m i915kms 2>/dev/null; then
    ok "i915kms already loaded"
else
    say "        loading i915kms ..."
    if kldload i915kms 2>>"$REPORT"; then
        ok "i915kms loaded successfully"
    else
        bad "i915kms refused to load — this is the blocking failure"
        note "the two usual causes, in order of likelihood:"
        note "  1. missing firmware  -> pkg install gpu-firmware-intel-kabylake"
        note "  2. package built for a different 14.x point release"
        note "     -> cd /usr/ports/graphics/drm-kmod && make install clean"
        note "check: dmesg | tail -30"
    fi
fi

# The definitive firmware check: the driver says so itself.
say "        firmware load status from dmesg:"
FW=$(dmesg 2>/dev/null | grep -i drm | grep -i 'firmware' | tail -5)
if [ -n "$FW" ]; then
    printf '%s\n' "$FW" | while read -r l; do note "$l"; done
    if printf '%s' "$FW" | grep -qi 'successfully loaded'; then
        ok "firmware loaded successfully"
    elif printf '%s' "$FW" | grep -qiE 'not found|failed|error'; then
        bad "firmware missing — read the filename above and install its package"
        note "the 'kbl_' prefix means Kaby Lake: gpu-firmware-intel-kabylake"
    fi
else
    note "no drm firmware lines in dmesg yet"
fi

# --- 4. the DRM device -----------------------------------------------------
say ""
say "[4] DRM device node"
if [ -c /dev/dri/card0 ]; then
    ok "/dev/dri/card0 exists"
    ls -l /dev/dri/ 2>/dev/null | while read -r l; do note "$l"; done
else
    bad "/dev/dri/card0 missing — the compositor has nothing to draw on"
fi

# --- 5. session management -------------------------------------------------
say ""
say "[5] seatd"
if pkg info -e seatd >/dev/null 2>&1; then
    ok "seatd installed"
    service seatd onestatus >/dev/null 2>&1 \
        && ok "seatd running" \
        || note "start it: sysrc seatd_enable=YES && service seatd start"
else
    note "not installed: pkg install seatd"
fi

# --- 6. the compositor -----------------------------------------------------
say ""
say "[6] compositor"
FOUND=""
for c in wayfire sway labwc; do
    if command -v "$c" >/dev/null 2>&1; then
        ok "$c is installed"
        [ -z "$FOUND" ] && FOUND="$c"
    fi
done
if [ -z "$FOUND" ]; then
    note "none installed. wayfire and sway ship as binary packages:"
    note "  pkg install wayfire wf-shell     # stacking, plugin/shader system"
    note "  pkg install sway                 # tiling, most mature on FreeBSD"
    note "labwc has no binary package; build it:"
    note "  cd /usr/ports/x11-wm/labwc && make install clean"
fi

# --- 7. the actual run -----------------------------------------------------
say ""
say "[7] launching the compositor"
if [ -n "$FOUND" ] && [ -c /dev/dri/card0 ]; then
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/hajime-rt}"
    mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
    say "        starting $FOUND for 15 seconds ..."
    ( "$FOUND" >/tmp/hajime_compositor.log 2>&1 & echo $! > /tmp/hajime_comp.pid )
    sleep 15
    CPID=$(cat /tmp/hajime_comp.pid 2>/dev/null)
    if [ -n "$CPID" ] && kill -0 "$CPID" 2>/dev/null; then
        ok "$FOUND stayed up for 15 seconds"
        ls "$XDG_RUNTIME_DIR"/wayland-* >/dev/null 2>&1 \
            && ok "a wayland socket was created" \
            || bad "no wayland socket: it started but is not serving"
        kill "$CPID" 2>/dev/null
    else
        bad "$FOUND exited early"
        note "log tail:"
        tail -15 /tmp/hajime_compositor.log 2>/dev/null | while read -r l; do note "$l"; done
    fi
else
    note "skipped: needs both a compositor and /dev/dri/card0"
fi

# --- verdict ---------------------------------------------------------------
say ""
say "============================================"
say "  passed: $PASS    failed: $FAIL"
say "============================================"
if [ "$FAIL" -eq 0 ] && [ "$PASS" -ge 5 ]; then
    say "VIABLE — a Wayland desktop can be built on this hardware."
    say "Next: theme wayfire, add waybar, style with pixel art."
    say ""
    say "full report: $REPORT"
    exit 0
else
    say "BLOCKED — $FAIL check(s) failed."
    say "Do not start building the desktop until these pass."
    say "The likely fix is rebuilding drm-kmod from ports against this kernel."
    say ""
    say "full report: $REPORT"
    exit 1
fi
