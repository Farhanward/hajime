#!/bin/sh
# =============================================================================
# Create the Hajime jails.
#
# One ZFS dataset per jail, FreeBSD base extracted into each, the loopback and
# NAT the three of them share, and the pf rules that decide what may talk to
# what.
#
# The dataset per jail is the reason any of this exists: it lets one service be
# snapshotted, updated and rolled back without touching the other two.
#
# Idempotent. A jail that already exists is left alone.
#
# Usage:  sh create_jails.sh [--dry-run] [--only <jail>]
# Exit:   0 done, 1 refused or failed, 2 not root
# =============================================================================

set -u

DRY=0
ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1 ;;
        --only)    shift; ONLY="${1:-}" ;;
        -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

JAILS="web data ai"
JAIL_ROOT=/jails
MIRROR="https://download.freebsd.org/releases"
WORK=/var/tmp/hajime-jail-base

WARNINGS=0
say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
ok()   { printf '   ok    %s\n' "$*"; }
warn() { printf '   warn  %s\n' "$*"; WARNINGS=$((WARNINGS + 1)); }
die()  { printf '\n   FAILED: %s\n' "$*" >&2; exit 1; }

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '   would  %s\n' "$*"
        return 0
    fi
    "$@"
}

[ "$(id -u)" -eq 0 ] || { echo "run as root: this creates datasets and jails" >&2; exit 2; }

say "Hajime jail setup"
[ "$DRY" -eq 1 ] && say "dry run: nothing will be changed"
[ -n "$ONLY" ] && { JAILS="$ONLY"; say "only: ${ONLY}"; }

# --- 1. preflight ----------------------------------------------------------
step "preflight"

command -v zfs >/dev/null 2>&1 || die "no zfs. The per-jail rollback these \
exist for needs one dataset per jail."

POOL=$(zpool list -H -o name 2>/dev/null | head -1)
[ -n "$POOL" ] || die "no ZFS pool"
ok "pool ${POOL}"

# The datasets are named after whatever this pool is called, and hajimectl
# reads the same name from the machine rather than assuming `zroot`. An earlier
# version hardcoded zroot in both places, which is right on a default FreeBSD
# install and silently wrong on any other: every jail reported as "not created"
# and every rollback had nothing to find.
if [ "$(zpool list -H -o name | wc -l | tr -d ' ')" -gt 1 ]; then
    warn "more than one pool; the jails will use '${POOL}'. Set HAJIME_POOL \
for hajimectl if that is not the one you meant."
fi

REL=$(freebsd-version -r 2>/dev/null | sed 's/-p[0-9]*$//')
case "$REL" in
    14.4*|15.*) ok "FreeBSD ${REL}" ;;
    *) die "release ${REL}: the jails must match the host base, and this script \
fetches ${REL} from the mirror" ;;
esac

FREE=$(zpool list -H -p -o free "$POOL")
FREE_GB=$((FREE / 1024 / 1024 / 1024))
# Roughly 4 GB per jail: 1.5 for the base, the rest for what gets installed
# into it. Scaled to what is actually being created, because `--only ai` asking
# for the space of three jails refuses a machine that has room for the one.
WANTED=$(echo $JAILS | wc -w | tr -d ' ')
NEED_GB=$((WANTED * 4))
[ "$FREE_GB" -ge "$NEED_GB" ] || die "only ${FREE_GB} GB free; ${WANTED} jail(s) need ${NEED_GB} GB"
ok "${FREE_GB} GB free, ${NEED_GB} GB needed for ${WANTED} jail(s)"

# --- 2. the base archive ---------------------------------------------------
# Fetched once and reused for all three, checked against the mirror's own
# manifest. An unverified base is a jail built from whatever arrived.
step "base system"

BASE="$WORK/base.txz"
if [ -f "$BASE" ]; then
    ok "base.txz already fetched"
else
    run mkdir -p "$WORK"
    URL="${MIRROR}/$(uname -m)/${REL}-RELEASE/base.txz"
    say "   fetching ${URL}"
    if run fetch -o "$BASE" "$URL"; then
        ok "base.txz fetched"
    else
        die "could not fetch the base archive from ${URL}"
    fi
fi

if [ "$DRY" -eq 0 ]; then
    MANIFEST="$WORK/MANIFEST"
    if [ ! -f "$MANIFEST" ]; then
        fetch -o "$MANIFEST" "${MIRROR}/$(uname -m)/${REL}-RELEASE/MANIFEST" \
            >/dev/null 2>&1 || warn "could not fetch the release MANIFEST"
    fi
    if [ -f "$MANIFEST" ]; then
        WANT=$(awk '$1 == "base.txz" { print $2 }' "$MANIFEST")
        GOT=$(sha256 -q "$BASE" 2>/dev/null || sha256sum "$BASE" | cut -d' ' -f1)
        if [ -n "$WANT" ] && [ "$WANT" = "$GOT" ]; then
            ok "base.txz matches the release manifest"
        elif [ -n "$WANT" ]; then
            rm -f "$BASE"
            die "base.txz does not match the release manifest. The download was \
corrupt or tampered with; it has been deleted."
        else
            warn "base.txz not listed in the MANIFEST; checksum unverified"
        fi
    else
        warn "base.txz checksum unverified"
    fi
fi

# --- 3. the jails ----------------------------------------------------------
step "jails"

addr_of() {
    case "$1" in
        web)  echo 10 ;;
        data) echo 20 ;;
        ai)   echo 30 ;;
        *)    echo "" ;;
    esac
}

for j in $JAILS; do
    ADDR=$(addr_of "$j")
    [ -n "$ADDR" ] || die "unknown jail '${j}'; known: web data ai"

    DATASET="${POOL}/jails/${j}"
    ROOT="${JAIL_ROOT}/${j}"

    if zfs list -H -o name "$DATASET" >/dev/null 2>&1; then
        ok "${j}: dataset exists, left alone"
        continue
    fi

    say "   ${j}: creating"
    # The parent is created once and holds nothing itself.
    if ! zfs list -H -o name "${POOL}/jails" >/dev/null 2>&1; then
        run zfs create -o mountpoint="${JAIL_ROOT}" "${POOL}/jails" \
            || die "could not create ${POOL}/jails"
    fi
    run zfs create "$DATASET" || die "could not create ${DATASET}"
    ok "   dataset ${DATASET} at ${ROOT}"

    if run tar -xf "$BASE" -C "$ROOT" 2>/dev/null; then
        ok "   base extracted"
    else
        die "could not extract the base into ${ROOT}"
    fi

    # Minimum a jail needs to boot and resolve names.
    if [ "$DRY" -eq 0 ]; then
        cp /etc/resolv.conf "$ROOT/etc/resolv.conf"
        cp /etc/localtime "$ROOT/etc/localtime" 2>/dev/null || true
        cat > "$ROOT/etc/rc.conf" <<RCEOF
# Hajime jail: ${j}
# Managed by create_jails.sh on creation; edit freely afterwards.
syslogd_flags="-ss"
sendmail_enable="NONE"
cron_flags="-J 60"
clear_tmp_enable="YES"
RCEOF
        # No services enabled here. Each is turned on when it is installed, so
        # a jail that failed to finish setting up does not come back at reboot
        # pretending to serve something.
    fi
    ok "   ${j} ready at 10.99.0.${ADDR}"

    # A snapshot of the untouched base, so a jail can be returned to the state
    # it was in before anything was installed into it.
    run zfs snapshot "${DATASET}@hajime-base-$(date +%Y%m%d-%H%M%S)" \
        && ok "   base snapshot taken"
done

# --- 4. the network --------------------------------------------------------
# A cloned loopback holds the jail addresses; pf translates their outbound
# traffic and decides which of them may reach what.
step "network"

run sysrc cloned_interfaces="lo1" >/dev/null
run sysrc ifconfig_lo1_name="jails" >/dev/null 2>&1 || true
ok "lo1 will be cloned at boot"

if [ "$DRY" -eq 0 ] && ! ifconfig lo1 >/dev/null 2>&1; then
    ifconfig lo1 create && ok "lo1 created now"
fi

for j in $JAILS; do
    ADDR=$(addr_of "$j")
    if [ "$DRY" -eq 0 ] && ifconfig lo1 2>/dev/null | grep -q "10.99.0.${ADDR}"; then
        ok "10.99.0.${ADDR} already on lo1"
    else
        run ifconfig lo1 inet "10.99.0.${ADDR}/32" alias \
            && ok "10.99.0.${ADDR} added to lo1"
    fi
done

PF_CONF=/etc/pf.conf
if [ -f "$PF_CONF" ] && grep -q "hajime jails" "$PF_CONF" 2>/dev/null; then
    ok "pf already carries the jail rules"
else
    EXT=$(ifconfig -l | tr ' ' '\n' | grep -v '^lo' | head -1)
    say "   writing jail rules for ${EXT} to ${PF_CONF}.hajime"
    if [ "$DRY" -eq 0 ]; then
        cat > "${PF_CONF}.hajime" <<PFEOF
# hajime jails
# Reviewed and merged into /etc/pf.conf by hand: this file is not loaded on its
# own, because a generated ruleset that replaces yours is how a machine loses
# its remote access.

ext_if = "${EXT}"
jail_net = "10.99.0.0/24"
web  = "10.99.0.10"
data = "10.99.0.20"
ai   = "10.99.0.30"

# Outbound translation, so the jails can fetch packages and reach the internet
# through the host's address.
nat on \$ext_if from \$jail_net to any -> (\$ext_if)

# The databases take connections from the other jails and from the host, and
# from nothing else. This is the rule that makes the split worth having.
block in quick on lo1 from ! { \$web, \$ai, 127.0.0.1 } to \$data

# The model reaches nothing on its own. The gateway on the host does the
# fetching, and the gateway writes down what it fetched. A model that can open
# its own connections can exfiltrate what it was shown.
block out quick from \$ai to ! { \$jail_net, 127.0.0.1 }
PFEOF
    fi
    ok "rules written to ${PF_CONF}.hajime"
    warn "review ${PF_CONF}.hajime and merge it into ${PF_CONF} yourself; \
this script will not overwrite a working firewall"
fi

# --- 5. verdict ------------------------------------------------------------
step "done"

if [ "$DRY" -eq 1 ]; then
    say "   dry run complete; nothing was changed"
    exit 0
fi

say "   ${WARNINGS} warning(s)."
say ""
say "   Next:"
say "     install -m 644 jail.conf /etc/jail.conf"
say "     sysrc jail_enable=YES"
say "     service jail start"
say "     hajimectl jails"
say ""
say "   Each jail has a base snapshot already. Before you change one:"
say "     hajimectl jail-snapshot ai before-model-swap"
exit 0
