#!/bin/sh
# =============================================================================
# Hajime OS installer for FreeBSD 14.4.
#
# Turns a stock FreeBSD install into the Hajime server: packages, tunables, the
# native Rust daemons, their rc.d scripts, and the service tree.
#
# Three properties matter more than speed here.
#
#   Idempotent.   Running it twice must leave the machine as it was after the
#                 first run. The tunables are written as a marked block that is
#                 replaced, never appended, because an installer that appends is
#                 an installer that cannot be re-run after a failure.
#
#   Reversible.   A boot environment is created before the first change. If the
#                 install goes wrong the machine boots back into the state it
#                 was in, which matters on a host with no IPMI and no console.
#
#   Honest.       Every step that fails says so and stops. Nothing is wrapped in
#                 `|| true` to keep the output green.
#
# Usage:  sh install_hajime_os.sh [--dry-run] [--skip-build] [--skip-packages]
#                                 [--no-be]
# Exit:   0 installed, 1 refused or failed, 2 not root
#
# --skip-packages is for a machine whose packages are managed elsewhere: an
# offline install, a prebuilt image, or a jail whose base already carries them.
# It does not check that they are there, so what it buys in flexibility it pays
# for in a later, less obvious failure when they are not.
# =============================================================================

set -u

DRY=0
SKIP_BUILD=0
SKIP_PACKAGES=0
MAKE_BE=1

for arg in "$@"; do
    case "$arg" in
        --dry-run)    DRY=1 ;;
        --skip-build)    SKIP_BUILD=1 ;;
        --skip-packages) SKIP_PACKAGES=1 ;;
        --no-be)         MAKE_BE=0 ;;
        -h|--help)
            sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 1 ;;
    esac
done

HERE=$(cd "$(dirname "$0")" && pwd)
HAJIME_USER=hajime
HAJIME_ETC=/usr/local/etc/hajime
HAJIME_DATA=/vault/hajime
BIN=/usr/local/bin
RCD=/usr/local/etc/rc.d
LOG=/var/log/hajime-install.log

WARNINGS=0
BLOCKERS=0
say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
ok()   { printf '   ok    %s\n' "$*"; }
warn() { printf '   warn  %s\n' "$*"; WARNINGS=$((WARNINGS + 1)); }

# A real run stops at the first blocker. A dry run notes it and keeps going: the
# point of a dry run is to see the whole plan, and one that halts on the first
# problem hides the four behind it.
die() {
    if [ "$DRY" -eq 1 ]; then
        printf '   WOULD REFUSE: %s\n' "$*"
        BLOCKERS=$((BLOCKERS + 1))
        return 0
    fi
    printf '\n   FAILED: %s\n' "$*" >&2
    exit 1
}

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '   would  %s\n' "$*"
        return 0
    fi
    "$@" >>"$LOG" 2>&1
}

[ "$(id -u)" -eq 0 ] || { echo "run as root: this installs packages and edits rc.conf" >&2; exit 2; }

say "Hajime OS installer"
say "source: ${HERE}"
[ "$DRY" -eq 1 ] && say "dry run: nothing will be changed"
[ "$DRY" -eq 0 ] && : >"$LOG"

# --- 1. preflight ----------------------------------------------------------
# Everything that would make the install fail halfway is checked before the
# first change. A machine left half-installed is worse than one not started.
step "preflight"

REL=$(freebsd-version -r 2>/dev/null || uname -r)
case "$REL" in
    14.4*|15.*) ok "FreeBSD ${REL}" ;;
    14.0*|14.1*|14.2*|14.3*)
        die "FreeBSD ${REL} is end-of-life. This host serves public sites; \
upgrade with 'freebsd-update upgrade -r 14.4-RELEASE' first." ;;
    *) warn "unrecognised release ${REL}; the package set assumes 14.4" ;;
esac

ARCH=$(uname -m)
[ "$ARCH" = "amd64" ] || die "architecture ${ARCH}; the binaries are built for amd64"
ok "${ARCH}"

# The workflow engine, the model and the databases all live on /vault. Without a
# pool there is also nothing to snapshot, and rollback is the whole safety net.
if zpool list -H -o name 2>/dev/null | grep -q .; then
    POOL=$(zpool list -H -o name | head -1)
    FREE=$(zpool list -H -p -o free "$POOL" 2>/dev/null || echo 0)
    FREE_GB=$((FREE / 1024 / 1024 / 1024))
    if [ "$FREE_GB" -lt 8 ]; then
        die "only ${FREE_GB} GB free on ${POOL}; the packages and the build need 8 GB"
    fi
    ok "zpool ${POOL}, ${FREE_GB} GB free"
else
    die "no ZFS pool. Hajime uses boot environments and snapshots to undo \
mistakes; on UFS there is nothing to roll back to."
fi

MEM_MB=$(($(sysctl -n hw.physmem) / 1024 / 1024))
ok "${MEM_MB} MB RAM"
if [ "$MEM_MB" -lt 7000 ]; then
    warn "under 8 GB: run 'hajimectl save' after the install so only the \
essential services stay up"
fi

if ! ping -c1 -t3 pkg.freebsd.org >/dev/null 2>&1; then
    warn "pkg.freebsd.org did not answer; package installs may fail"
fi

# --- 2. a way back ---------------------------------------------------------
step "rollback point"

BE=""
if [ "$MAKE_BE" -eq 0 ]; then
    warn "boot environment skipped by request; there is no way back from here"
elif command -v bectl >/dev/null 2>&1; then
    BE="hajime-preinstall-$(date +%Y%m%d-%H%M%S)"
    if run bectl create "$BE"; then
        ok "boot environment ${BE}"
        say "         to undo everything below:  bectl activate ${BE} && reboot"
    else
        die "could not create a boot environment. Fix that first, or pass \
--no-be if you accept having no way back."
    fi
else
    warn "bectl not found; no boot environment was created"
fi

# --- 3. user, groups and directories ---------------------------------------
# The daemons run unprivileged. Only the installer needs root.
step "user and directories"

if pw usershow "$HAJIME_USER" >/dev/null 2>&1; then
    ok "user ${HAJIME_USER} exists"
else
    run pw groupadd "$HAJIME_USER" -g 900
    if run pw useradd "$HAJIME_USER" -u 900 -g "$HAJIME_USER" -d "$HAJIME_DATA" \
            -s /usr/sbin/nologin -c "Hajime services"; then
        ok "user ${HAJIME_USER} created (uid 900, nologin)"
    else
        die "could not create the ${HAJIME_USER} user"
    fi
fi

# 750 on the data directory: it holds workflow definitions and the history log,
# and the history quotes request bodies.
for d in "$HAJIME_DATA" "$HAJIME_DATA/workflows" "$HAJIME_DATA/models"; do
    run install -d -o "$HAJIME_USER" -g "$HAJIME_USER" -m 750 "$d" \
        && ok "$d" || die "could not create $d"
done

# Config is root-owned, group-readable by hajime: the daemons read the tokens,
# they do not get to rewrite them.
run install -d -o root -g "$HAJIME_USER" -m 750 "$HAJIME_ETC" \
    && ok "$HAJIME_ETC" || die "could not create $HAJIME_ETC"

# --- 4. packages -----------------------------------------------------------
step "packages"

# postgresql17 matches the version Postiz ran, so its dump restores without a
# version jump. mariadb1011 matches LiteCart and Nginx Proxy Manager.
# No openssl here: FreeBSD's base system ships /usr/bin/openssl, which is all
# the token generation below needs. Installing the port puts a second, usually
# different, openssl in /usr/local/bin and is a well-known way to end up with
# ports linked against one version and running against another.
PKGS="postgresql17-server postgresql17-client mariadb1011-server mariadb1011-client \
redis caddy cloudflared ca_root_nss curl"

if [ "$SKIP_BUILD" -eq 0 ]; then
    # llvm supplies libclang, which rquickjs needs to generate bindings on
    # FreeBSD. On Windows the crate ships them; here it does not.
    PKGS="$PKGS rust go git llvm19"
fi

if [ "$SKIP_PACKAGES" -eq 1 ]; then
    warn "packages skipped by request; nothing below checks that they are present"
    say "   expected: $(echo $PKGS | tr ' ' '
' | wc -l | tr -d ' ') package(s)"
    PKGS=""
elif [ "$DRY" -eq 0 ]; then
    run pkg update || warn "pkg update failed; installing from the local cache"
fi

for p in $PKGS; do
    if pkg info -e "$p" 2>/dev/null; then
        ok "$p already installed"
    elif [ "$DRY" -eq 1 ]; then
        say "   would  pkg install -y $p"
    else
        printf '   install %s ... ' "$p"
        if run pkg install -y "$p"; then
            printf 'done\n'
        else
            printf 'FAILED\n'
            die "could not install ${p}; see ${LOG}. Nothing further was changed."
        fi
    fi
done

# --- 5. tunables -----------------------------------------------------------
# Written as a marked block so a second run replaces it rather than stacking a
# second copy. Each value goes in the file that actually reads it: the ARC cap
# and the idle method are loader tunables, interface offloads are ifconfig
# flags, and the CPU idle floor belongs to power_profile.
step "tunables"

BEGIN="# --- BEGIN hajime ---"
END="# --- END hajime ---"

write_block() {
    file="$1"; body="$2"
    if [ "$DRY" -eq 1 ]; then
        printf '   would  rewrite the hajime block in %s\n' "$file"
        return 0
    fi
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

# ARC is capped because the databases, the model and the workflow engine all
# want the same 8 GB. Uncapped ARC on this host starves them and the machine
# swaps, which is what the old stack did.
ARC_MB=$((MEM_MB / 4))
[ "$ARC_MB" -gt 2048 ] && ARC_MB=2048

LOADER_BODY="# Managed by install_hajime_os.sh. Edits inside this block are lost
# on the next run; put your own settings outside it.
vfs.zfs.arc.max=\"${ARC_MB}M\"
vfs.zfs.prefetch_disable=\"0\"
# mwait idles a core without dropping into a deep C-state, so an arriving
# request does not pay the wake-up latency. It costs a few watts on a machine
# that never sleeps anyway.
machdep.idle=\"mwait\"
"
write_block /boot/loader.conf "$LOADER_BODY" \
    && ok "loader.conf: ARC capped at ${ARC_MB} MB, mwait idle" \
    || die "could not write /boot/loader.conf"

SYSCTL_BODY="# Managed by install_hajime_os.sh.
# Socket buffers sized for a host that mostly serves TLS to the internet.
net.inet.tcp.sendspace=131072
net.inet.tcp.recvspace=131072
"
write_block /etc/sysctl.conf "$SYSCTL_BODY" \
    && ok "sysctl.conf: TCP buffers" \
    || die "could not write /etc/sysctl.conf"

# The CPU idle floor goes through power_profile rather than sysctl.conf: the raw
# oid is reset whenever the profile changes, and sysctl.conf runs before the cpu
# devices are guaranteed to have attached.
run sysrc performance_cx_lowest="C1" >/dev/null
run sysrc economy_cx_lowest="C1" >/dev/null
ok "CPU idle floor set to C1 through power_profile"

# hwpstate_intel exists only if the driver attached. A missing oid in
# sysctl.conf prints an error on every boot, so it is set here if present.
if sysctl -n dev.hwpstate_intel.0.epp >/dev/null 2>&1; then
    run sysctl dev.hwpstate_intel.0.epp=0 >/dev/null
    ok "Intel HWP biased to performance"
else
    say "   note  hwpstate_intel not attached; frequency scaling left alone"
fi

# Interface offloads are per-interface ifconfig flags. The tunables the first
# draft of this script set (hw.re.tso and friends) do not exist in re(4).
NIC=$(ifconfig -l | tr ' ' '\n' | grep -v '^lo' | head -1)
if [ -n "$NIC" ]; then
    CUR=$(sysrc -n "ifconfig_${NIC}" 2>/dev/null || echo "DHCP")
    case "$CUR" in
        *tso*) ok "${NIC} offloads already configured" ;;
        *)
            # Checksums on, TSO off: TSO in re(4) has a long history of
            # corrupting large segments, and this host terminates TLS.
            run sysrc "ifconfig_${NIC}=${CUR} -tso4 -tso6 rxcsum txcsum" >/dev/null
            ok "${NIC}: checksum offload on, TSO off"
            ;;
    esac
else
    warn "no non-loopback interface found; network tuning skipped"
fi

# --- 6. the native tools ---------------------------------------------------
step "native tools"

BINARIES="hajime-workflow hajime-ai hajime-wa hajime-console hajimectl hajime-model"

if [ "$SKIP_BUILD" -eq 1 ]; then
    ok "build skipped; expecting binaries in ${HERE}/target/release"
elif [ "$DRY" -eq 1 ]; then
    say "   would  cargo build --release --workspace"
else
    say "   building the workspace; this takes a few minutes"
    if ! ( cd "$HERE" && cargo build --release --workspace >>"$LOG" 2>&1 ); then
        die "the build failed; see ${LOG}"
    fi
    ok "workspace built"

    if [ -d "$HERE/hajime-wa/bridge" ]; then
        if ( cd "$HERE/hajime-wa/bridge" && go build -o hajime-wa-bridge . >>"$LOG" 2>&1 ); then
            ok "whatsapp bridge built"
        else
            warn "the Go bridge did not build; WhatsApp will be unavailable"
        fi
    fi
fi

for b in $BINARIES; do
    src="$HERE/target/release/$b"
    if [ "$DRY" -eq 1 ]; then
        say "   would  install $b to $BIN"
        continue
    fi
    [ -x "$src" ] || die "$src is missing. Build first, or drop --skip-build."
    run install -o root -g wheel -m 755 "$src" "$BIN/$b" \
        && ok "$b" || die "could not install $b"
done

if [ -x "$HERE/hajime-wa/bridge/hajime-wa-bridge" ] && [ "$DRY" -eq 0 ]; then
    run install -o root -g wheel -m 755 \
        "$HERE/hajime-wa/bridge/hajime-wa-bridge" "$BIN/hajime-wa-bridge" \
        && ok "hajime-wa-bridge"
fi

# --- 7. rc.d scripts and tokens --------------------------------------------
step "services"

# Every service that has an rc script. The console and the WhatsApp bridge were
# missing from this list, so both were installed as binaries that nothing ever
# started: the console answered nothing on 8088 while the message of the day
# pointed at it, and the bridge left the gateway reporting "bridge down" for
# ever. hajime-wa/rc.d holds two scripts, which is why the inner loop exists.
for svc in hajime-workflow hajime-ai hajime-wa hajime-console; do
    name=$(echo "$svc" | tr '-' '_')
    src="$HERE/$svc/rc.d/$name"
    [ -f "$src" ] || { warn "$src not found"; continue; }
    # FreeBSD's sh rejects CRLF with a syntax error that names the wrong line.
    # This has bitten twice already, so check rather than trust the checkout.
    if [ "$DRY" -eq 0 ] && grep -q "$(printf '\r')" "$src"; then
        die "$src has CRLF line endings; FreeBSD's sh will not parse it."
    fi
    run install -o root -g wheel -m 755 "$src" "$RCD/$name" \
        && ok "rc.d/$name" || die "could not install rc.d/$name"
done

# One token per service: 32 bytes from the kernel generator, 640 root:hajime so
# the daemons can read them and nothing else can. An existing token is left
# alone, or a re-run would lock out whatever already holds it.
new_token() {
    target="$1"
    if [ "$DRY" -eq 1 ]; then
        printf '   would  generate %s\n' "$target"
        return 0
    fi
    if [ -s "$target" ]; then
        ok "$(basename "$target") already present, left alone"
        return 0
    fi
    # Base openssl by explicit path. Whichever openssl a later package puts on
    # PATH, the token must come from the one shipped with the system.
    ( umask 077; /usr/bin/openssl rand -base64 32 | tr -d '\n' > "$target" ) || return 1
    chown root:"$HAJIME_USER" "$target" || return 1
    chmod 640 "$target" || return 1
    ok "$(basename "$target") generated"
}

for t in token ai_token wa_token; do
    new_token "$HAJIME_ETC/$t" || die "could not generate $HAJIME_ETC/$t"
done

# The system model's weights, trained once here rather than on every
# invocation. Training takes a fraction of a second either way; doing it at
# install time means the machine runs exactly the weights that were measured.
if [ "$DRY" -eq 1 ]; then
    say "   would  train the system model into $HAJIME_ETC/model.json"
elif [ -x "$BIN/hajime-model" ]; then
    if "$BIN/hajime-model" train "$HAJIME_ETC/model.json" >>"$LOG" 2>&1; then
        chmod 644 "$HAJIME_ETC/model.json"
        ok "system model trained into $HAJIME_ETC/model.json"
        run sysrc hajime_model_weights="$HAJIME_ETC/model.json" >/dev/null
    else
        warn "the system model did not train; hajime-model will train on each run"
    fi
fi

# --- 8. database clusters --------------------------------------------------
# A freshly installed postgresql has no data directory, and `service postgresql
# start` on one fails with a message about initdb that is easy to read as a
# broken package. The cluster is created here so the first start works, and the
# restore has somewhere to restore into.
step "database clusters"

PG_DATA=$(sysrc -n postgresql_data 2>/dev/null || echo /var/db/postgres/data17)
if ! pkg info -e postgresql17-server 2>/dev/null; then
    warn "postgresql is not installed, so there is no cluster to create"
elif [ "$DRY" -eq 1 ]; then
    say "   would  service postgresql initdb"
elif [ -f "$PG_DATA/PG_VERSION" ]; then
    ok "postgresql cluster already initialised at ${PG_DATA}"
else
    # initdb refuses a non-empty directory, which is the behaviour we want:
    # silently reinitialising over an existing cluster would destroy it.
    if run service postgresql initdb; then
        ok "postgresql cluster created at ${PG_DATA}"
    else
        warn "postgresql initdb failed; see ${LOG}. Restore will have nowhere to go."
    fi
fi

# mariadb builds its system tables on first start, so it needs no equivalent
# step. What it does need is a data directory it owns.
if ! pkg info -e mariadb1011-server 2>/dev/null; then
    warn "mariadb is not installed, so its data directory is not created"
elif [ "$DRY" -eq 1 ]; then
    say "   would  create /var/db/mysql"
elif [ -d /var/db/mysql ]; then
    ok "mariadb data directory present"
else
    run install -d -o mysql -g mysql -m 700 /var/db/mysql \
        && ok "mariadb data directory created" \
        || warn "could not create /var/db/mysql"
fi

# Enable order follows the table in hajime-sys: databases and the proxy come up
# before anything that talks to them. The optional tier stays off, so a reboot
# on a tight machine comes back serving sites rather than loading a model.
step "boot order"

for s in zfs_enable postgresql_enable mysql_enable redis_enable \
         caddy_enable cloudflared_enable hajime_workflow_enable; do
    run sysrc "${s}=YES" >/dev/null && ok "${s}=YES"
done

# Loopback only: Caddy terminates TLS in front of it. Binding the engine to the
# public address would expose the control endpoints next to the webhooks.
run sysrc hajime_workflow_bind="127.0.0.1:5678" >/dev/null
run sysrc hajime_workflow_token_file="$HAJIME_ETC/token" >/dev/null
run sysrc hajime_workflow_workflows="$HAJIME_DATA/workflows.json" >/dev/null
run sysrc hajime_workflow_history="$HAJIME_DATA/history.jsonl" >/dev/null
run sysrc hajime_workflow_tz="Asia/Riyadh" >/dev/null
ok "workflow engine on loopback, Riyadh time"

# The host-touching node types stay off. Two of the imported workflows pass a
# webhook body to a shell, and a webhook path is reachable by anyone who learns
# it.
run sysrc hajime_workflow_allow_command="NO" >/dev/null
run sysrc hajime_workflow_allow_ssh="NO" >/dev/null
ok "executeCommand and ssh nodes disabled"

# The console is the exception among the optional services: it costs 15 MB and
# it is how you find out what the others are doing, so leaving it off would mean
# the first thing a new machine asks you to do is start the thing that tells you
# what to start.
run sysrc hajime_console_enable="YES" >/dev/null
ok "console enabled: http://127.0.0.1:8088/"

for s in hajime_ai_enable hajime_wa_enable hajime_wa_bridge_enable; do
    run sysrc "${s}=NO" >/dev/null
done
ok "optional services left off; start one with 'hajimectl start hajime_ai'"

# --- 7a. the sites ---------------------------------------------------------
# Caddy and cloudflared were installed and never configured, which is a machine
# that passes every step of this installer and serves nothing. Neither is
# written blind: the site table decides the proxy, and the tunnel needs a
# credential this script will not invent.
step "sites"

if [ -x /usr/local/sbin/php-fpm ] || [ -x /usr/local/bin/php-fpm ]; then
    run sysrc php_fpm_enable="YES" >/dev/null && ok "php-fpm enabled"
    if [ "$DRY" -eq 0 ] && [ ! -f /usr/local/etc/php.ini ] &&        [ -f /usr/local/etc/php.ini-production ]; then
        run install -m 644 /usr/local/etc/php.ini-production /usr/local/etc/php.ini             && ok "php.ini from the production template"
    fi
else
    warn "php-fpm not found; a php site would be refused by the generator"
fi

if [ -f "${HERE}/hajime-web/generate_caddyfile.sh" ]; then
    GEN_ARGS=""
    [ "$DRY" -eq 1 ] && GEN_ARGS="--dry-run"
    # shellcheck disable=SC2086
    sh "${HERE}/hajime-web/generate_caddyfile.sh" $GEN_ARGS
    case $? in
        0) ok "Caddyfile generated from hajime-web/sites.conf"
           run sysrc caddy_enable="YES" >/dev/null ;;
        2) warn "no sites declared in hajime-web/sites.conf.
            Caddy is installed and will serve nothing until you fill it in.
            This is the step that brings the websites back." ;;
        *) warn "the Caddyfile was not generated; its output above says why" ;;
    esac
fi

if [ -f /usr/local/etc/cloudflared/config.yml ]; then
    run sysrc cloudflared_enable="YES" >/dev/null && ok "cloudflared enabled"
else
    warn "no /usr/local/etc/cloudflared/config.yml.
            The tunnel is the only path in from outside, and its credential is
            an account secret this installer will not invent. The template and
            the three commands that produce one:
            hajime-web/cloudflared.yml.example"
fi

# --- 7b. the theme ---------------------------------------------------------
# Run here rather than left to the desktop installer, because most of what it
# sets is not a desktop thing: the loader's screen, the sixteen colours the
# kernel console prints in, the message of the day, and the language classes.
# A headless server gets all of that; --no-desktop skips the rest.
step "theme"
if [ -x "${HERE}/hajime-brand/install_theme.sh" ] || [ -f "${HERE}/hajime-brand/install_theme.sh" ]; then
    THEME_ARGS="--no-desktop"
    [ "$DRY" -eq 1 ] && THEME_ARGS="${THEME_ARGS} --dry-run"
    # shellcheck disable=SC2086
    if sh "${HERE}/hajime-brand/install_theme.sh" $THEME_ARGS; then
        ok "loader screen, console palette, motd and language classes"
    else
        warn "the theme installer refused; the system is installed and unstyled.
            Its own output above says why. Re-run it alone:
            sh hajime-brand/install_theme.sh"
    fi
else
    warn "hajime-brand/install_theme.sh not found; skipping the theme"
fi

# --- 8. verdict ------------------------------------------------------------
step "done"

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

say "   Installed, ${WARNINGS} warning(s). Full log: ${LOG}"
say ""
say "   Nothing is running yet and no data has been restored. In order:"
say ""
say "     1. restore the data     sh hajime-migrate/restore_data.sh <backup-dir>"
say "     2. check the machine    hajimectl check"
say "     3. bring services up    hajimectl start"
say "     4. reboot, so the tunables load and you learn now whether it"
say "        comes back clean rather than during the next power cut"
say ""
say "   Two more layers, both separate and both optional. Neither is needed to"
say "   serve the sites, and each can be added later:"
say ""
say "     jails    sh hajime-jails/create_jails.sh"
say "              three jails on their own datasets, so one service can be"
say "              rolled back without touching the other two"
say ""
say "     desktop  sh hajime-wm/install_desktop.sh"
say "              wayfire, the pixel theme, the wallpaper and the launchers"
say ""
say "   The system model is installed and can be asked things directly:"
say "     hajime-model ask \"what is running\""
say "     hajime-model explain postgresql"
say ""
if [ -n "$BE" ]; then
    say "   If the reboot goes wrong, from the boot menu or single-user:"
    say "     bectl activate ${BE} && reboot"
fi
exit 0
