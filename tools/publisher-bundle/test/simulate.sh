#!/bin/sh
# Exercises the bundle's Makefile and scripts end to end without a real
# Linux service manager, NATS or Claude Code: everything runs in a throwaway
# HOME (with a space in its path) against stubs of cc-hook, cc-publish,
# systemctl, journalctl, loginctl, uname and claude.
#
#   sh tools/publisher-bundle/test/simulate.sh            (GNU make on PATH)
#   MAKE=/path/to/make SIM_SH=dash sh .../simulate.sh     (scripts run by dash)
#
# Runs on Linux, and on Windows under Git Bash with a GNU make; there file
# modes are not real, so the mode check is skipped. Exits non-zero on the
# first failed assertion. Never touches the real HOME.
# `[ a ] && [ b ] || fail` is deliberate here: fail exits.
# shellcheck disable=SC2015,SC1091
set -eu

SRC=$(cd "$(dirname "$0")/.." && pwd)
MAKE=${MAKE:-make}
REAL_OS=$(uname -s)
SKIP_MODES=0
case $REAL_OS in MINGW* | MSYS* | CYGWIN*) SKIP_MODES=1 ;; esac

T=$(mktemp -d "${TMPDIR:-/tmp}/cc-bundle-sim.XXXXXX")
cleanup() {
    # Stop a publisher the profile test may have left running.
    if [ -f "$T/home dir/.local/share/cc-usage/cc-publish.pid" ]; then
        kill "$(cat "$T/home dir/.local/share/cc-usage/cc-publish.pid")" 2>/dev/null || true
    fi
    rm -rf "$T"
}
trap cleanup EXIT

pass=0
ok() { pass=$((pass + 1)); printf 'ok %d - %s\n' "$pass" "$*"; }
fail() { printf 'not ok - %s\n' "$*" >&2; [ -f "$T/last.log" ] && sed 's/^/    | /' "$T/last.log" >&2; exit 1; }

# ------------------------------------------------------------------ stubs
STUB=$T/stubs
mkdir -p "$STUB" "$T/state"
export SIM_STATE="$T/state" SIM_ARCH="${SIM_ARCH:-x86_64}"

cat >"$STUB/uname" <<'EOF'
#!/bin/sh
case ${1:-} in
    -s | "") echo Linux ;;
    -m) echo "$SIM_ARCH" ;;
    -r) echo 6.8.0-sim ;;
    *) echo "Linux sim 6.8.0-sim $SIM_ARCH" ;;
esac
EOF

# od: the stub binaries are shell scripts, so report the ELF header they
# declare in a "# fake-elf <machine>" line; anything else goes to the real od.
REAL_OD=$(command -v od)
cat >"$STUB/od" <<EOF
#!/bin/sh
for last; do :; done
m=\$(sed -n 's/^# fake-elf //p' "\$last" 2>/dev/null | head -n 1)
if [ -n "\$m" ]; then
    case "\$*" in
        *"-j 18 -N 2"*) printf ' %s %s\n' "\$(echo "\$m" | cut -c1-2)" "\$(echo "\$m" | cut -c3-4)" ;;
        *"-N 4"*) echo ' 7f 45 4c 46' ;;
    esac
    exit 0
fi
exec "$REAL_OD" "\$@"
EOF

cat >"$STUB/systemctl" <<'EOF'
#!/bin/sh
# Records calls; keeps enabled/active state in $SIM_STATE.
echo "systemctl $*" >>"$SIM_STATE/calls.log"
[ "${1:-}" = --user ] || { echo "stub: only --user" >&2; exit 1; }
shift
[ -f "$SIM_STATE/no-systemd" ] && { echo "System has not been booted with systemd as init system (PID 1). Can't operate." >&2; exit 1; }
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
case ${1:-} in --no-pager) shift ;; esac
cmd=${1:-}
shift || true
case $cmd in
    show-environment) echo "HOME=$HOME" ;;
    daemon-reload) : ;;
    enable) [ -f "$unit_dir/$1" ] || { echo "Unit $1 not found." >&2; exit 1; }; : >"$SIM_STATE/enabled" ;;
    disable)
        rm -f "$SIM_STATE/enabled"
        [ "${1:-}" = --now ] && rm -f "$SIM_STATE/active"
        ;;
    start | restart)
        [ -f "$unit_dir/$1" ] || { echo "Unit $1 not found." >&2; exit 1; }
        # The unit must carry an ExecStart that points at an installed binary.
        exe=$(sed -n 's/^ExecStart="\([^"]*\)".*/\1/p' "$unit_dir/$1")
        [ -x "$exe" ] || { echo "stub: ExecStart binary $exe missing" >&2; exit 1; }
        if grep -q '@[A-Z_]*@' "$unit_dir/$1"; then echo "stub: unrendered placeholder" >&2; exit 1; fi
        : >"$SIM_STATE/active"
        echo "published host heartbeat (sim restart)" >>"$SIM_STATE/journal.log"
        ;;
    stop) rm -f "$SIM_STATE/active" ;;
    is-active) if [ -f "$SIM_STATE/active" ]; then echo active; else echo inactive; exit 3; fi ;;
    is-enabled) if [ -f "$SIM_STATE/enabled" ]; then echo enabled; else echo disabled; exit 1; fi ;;
    show) echo "Mon 2026-10-12 10:00:00 UTC" ;;
    status) echo "* cc-publish.service - cc-usage publisher (sim)"; [ -f "$SIM_STATE/active" ] && echo "   Active: active (running)" ;;
    reset-failed) : ;;
    *) echo "stub systemctl: unsupported $cmd" >&2; exit 1 ;;
esac
EOF

cat >"$STUB/journalctl" <<'EOF'
#!/bin/sh
cat "$SIM_STATE/journal.log" 2>/dev/null || true
EOF

cat >"$STUB/loginctl" <<'EOF'
#!/bin/sh
echo "loginctl $*" >>"$SIM_STATE/calls.log"
case ${1:-} in
    show-user) if [ -f "$SIM_STATE/linger" ]; then echo yes; else echo no; fi ;;
    enable-linger) : >"$SIM_STATE/linger" ;;
esac
EOF

mkdir -p "$T/claudebin"
printf '#!/bin/sh\necho "claude stub"\n' >"$T/claudebin/claude"

# id: `id -u` answers $SIM_UID when set (bundle files owned by someone else,
# or running as root); everything else goes to the real id.
REAL_ID=$(command -v id)
cat >"$STUB/id" <<EOF
#!/bin/sh
if [ "\${1:-}" = -u ] && [ -n "\${SIM_UID:-}" ]; then echo "\$SIM_UID"; exit 0; fi
exec "$REAL_ID" "\$@"
EOF

# cc-hook stub: the installer subcommands, writing a marker into settings.json
# (the file given with --settings, else settings.json in CLAUDE_CONFIG_DIR or
# ~/.claude, like the real cc-hook).
cat >"$T/cc-hook" <<'EOF'
#!/bin/sh
# fake-elf 3e00
settings=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json
[ "${2:-}" = --settings ] && settings=$3
me=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
case ${1:-} in
    --version) echo "cc-hook 0.1.0" ;;
    install)
        mkdir -p "$(dirname "$settings")"
        [ -f "$settings" ] && cp "$settings" "$settings.bak-sim"
        printf '{ "hooks": "\\"%s\\"", "statusLine": "chained" }\n' "$me" >"$settings"
        echo "installing \"$me\" into $settings"
        ;;
    uninstall)
        echo '{}' >"$settings"
        echo "removing cc-hook from $settings"
        ;;
    status)
        echo "settings: $settings"
        if [ ! -f "$settings" ]; then echo "file does not exist; cc-hook is not installed"; exit 0; fi
        cmd=$(sed -n 's/.*"hooks": "\(.*\)", "statusLine".*/\1/p' "$settings" | sed 's/\\"/"/g')
        for e in SessionStart UserPromptSubmit PreToolUse Notification Stop SessionEnd; do
            if [ -n "$cmd" ]; then
                printf '  %-20s installed\n  %-20s   %s\n' "$e" "" "$cmd"
            else
                printf '  %-20s missing\n' "$e"
            fi
        done
        if [ -n "$cmd" ]; then
            printf '  %-20s chained: %s statusline\n  %-20s   then runs node statusline.js\n' statusLine "$cmd" ""
        else
            printf '  %-20s not chained (no statusLine command)\n' statusLine
        fi
        ;;
    *) exit 0 ;;
esac
EOF

# cc-publish stub: check validates the config; run publishes every second.
cat >"$T/cc-publish" <<'EOF'
#!/bin/sh
# fake-elf 3e00
get() { sed -n 's/^[[:space:]]*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*$/\1/p' "$2" | head -n 1; }
# Like the real one: unknown config members are an error, and the data dir
# comes from CC_USAGE_DATA_DIR.
members_ok() {
    bad=$(sed -n 's/^[[:space:]]*"\([^"]*\)"[[:space:]]*:.*/\1/p' "$1" | grep -vxE 'alias|servers|bucket|creds|publisher_key|viewer_pub|log_file' || true)
    [ -z "$bad" ] || { echo "invalid config: unknown field $bad" >&2; return 1; }
}
strict() {
    members_ok "$1" || exit 1
    [ -n "${CC_USAGE_DATA_DIR:-}" ] || { echo "CC_USAGE_DATA_DIR not set" >&2; exit 1; }
}
case ${1:-} in
    --version) echo "cc-publish 0.1.0" ;;
    check)
        cfg=$3
        strict "$cfg"
        printf '%s\n' "${CLAUDE_CONFIG_DIR:-<unset>}" >"$SIM_STATE/check.claude"
        # Both key files are required.
        for k in creds publisher_key viewer_pub; do
            p=$(get "$k" "$cfg")
            case $p in /*) ;; *) echo "check failed: $k path $p is not absolute" >&2; exit 1 ;; esac
            [ -f "$p" ] || { echo "check failed: $k $p missing" >&2; exit 1; }
        done
        echo "check passed: connected to wss://connect.ngs.global, bound cc-sessions, probe put/delete ok"
        ;;
    keygen)
        # keygen [--if-missing | --rotate | --force] --key PATH [--config CFG]
        # [--confirm ALIAS], like the real one: --if-missing keeps a key that
        # loads and prints its public line, a key that does not load is an
        # error, and replacing one needs --confirm with the config's alias
        # (exit 2). Every new key gets the next id (p1, p2, ...); --rotate
        # puts it in front of the old ones. Fixed key bytes: this is a stub.
        shift
        key= mode= cfg= confirm=
        while [ $# -gt 0 ]; do
            case $1 in
                --if-missing | --rotate | --force) mode=$1 ;;
                --key) key=$2; shift ;;
                --config) cfg=$2; shift ;;
                --confirm) confirm=$2; shift ;;
            esac
            shift
        done
        [ -n "$key" ] || { echo "stub keygen: --key needed" >&2; exit 2; }
        loads() { grep -qxE 'p[0-9]+:C{43}=' "$key"; }
        if [ -f "$key" ]; then
            case $mode in
                --if-missing)
                    loads || { echo "cannot load $key" >&2; exit 1; }
                    echo "$(sed -n '1s/:.*//p' "$key"):DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD="
                    exit 0
                    ;;
                "") echo "$key already exists; refusing to replace it" >&2; exit 1 ;;
            esac
            if [ -z "$cfg" ] || [ "$confirm" != "$(get alias "$cfg")" ]; then
                echo "This replaces the identity; confirm it with --confirm <alias>" >&2
                exit 2
            fi
            if [ "$mode" = --rotate ] && ! loads; then echo "cannot load $key" >&2; exit 1; fi
        fi
        n=$(($(cat "$SIM_STATE/kid" 2>/dev/null || echo 0) + 1))
        echo "$n" >"$SIM_STATE/kid"
        mkdir -p "$(dirname "$key")"
        if [ "$mode" = --rotate ] && [ -f "$key" ]; then
            { echo "p$n:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC="; cat "$key"; } >"$key.new"
        else
            echo "p$n:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=" >"$key.new"
        fi
        mv -f "$key.new" "$key"
        echo "p$n:DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD="
        ;;
    status)
        [ -f "$SIM_STATE/no-status-cmd" ] && { echo "unknown command status" >&2; exit 2; }
        # Like the real one: the config row is the path for a config it loads.
        if [ "${2:-}" = --config ]; then
            if members_ok "$3" 2>/dev/null && grep -q '"alias"' "$3"; then printf 'config      %s\n' "$3"; else echo "config      invalid config"; fi
        fi
        if [ -f "$SIM_STATE/active" ] || [ -f "$SIM_STATE/run.pid" ]; then echo "last publish 3s ago"; else echo "not running"; exit 1; fi
        ;;
    run)
        echo $ >"$SIM_STATE/run.pid"
        strict "$3"
        printf '%s\n' "${CLAUDE_CONFIG_DIR:-<unset>}" >"$SIM_STATE/run.claude"
        while :; do echo "published host heartbeat"; sleep 1; done
        ;;
esac
EOF
chmod +x "$STUB"/* "$T/claudebin/claude" "$T/cc-hook" "$T/cc-publish"

# A dash (or other) shell as `sh`, to catch bashisms in the scripts.
if [ -n "${SIM_SH:-}" ]; then
    sh_path=$(command -v "$SIM_SH") || { echo "SIM_SH=$SIM_SH not found" >&2; exit 2; }
    printf '#!/bin/sh\nexec "%s" "$@"\n' "$sh_path" >"$STUB/sh"
    chmod +x "$STUB/sh"
fi

# ------------------------------------------------------------------ bundle
B=$T/bundle/cc-usage-publisher-sim
mkdir -p "$B/bin" "$B/secrets" "$B/config" "$B/scripts" "$B/systemd"
cp "$SRC/Makefile" "$SRC/README.md" "$B/"
cp "$SRC"/scripts/*.sh "$B/scripts/"
cp "$SRC/systemd/cc-publish.service.in" "$B/systemd/"
cp "$T/cc-hook" "$T/cc-publish" "$B/bin/"
printf -- '-----BEGIN NATS USER JWT-----\nsim\n------END NATS USER JWT------\n' >"$B/secrets/cc-sessions.pub-sim.creds"
# The viewer's public key. No publisher key: the machine
# makes its own on the first install.
printf 'k1:FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFE=\n' >"$B/config/viewer.pub"
cat >"$B/config/publish.json" <<'EOF'
{
  "alias": "sim",
  "servers": ["wss://connect.ngs.global", "tls://connect.ngs.global"],
  "creds": "nats/cc-sessions.pub-sim.creds",
  "publisher_key": "keys/publisher.key",
  "viewer_pub": "keys/viewer.pub",
  "log_file": false
}
EOF
printf 'BUNDLE_FORMAT=1\nALIAS=sim\nNATS_USER=cc-sessions.pub-sim\nARCH=x86_64\nTARGET=x86_64-unknown-linux-musl\nVERSION=0.1.0\nCREATED=2026-10-13T00:00:00Z\n' >"$B/config/bundle.env"
bundle_sums() {
    (cd "$B" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort | while read -r f; do sha256sum "$f"; done) >"$T/SHA256SUMS.tmp"
    mv "$T/SHA256SUMS.tmp" "$B/SHA256SUMS"
}
bundle_sums

# ------------------------------------------------------------------ environment
H="$T/home dir"
mkdir -p "$H/.claude"
printf '# existing profile\nexport FOO=1\n' >"$H/.profile"
P=$H/.local/share/cc-usage
UNIT=$H/.config/systemd/user/cc-publish.service
PATH=$STUB:$T/claudebin:$PATH
export HOME="$H" PATH
unset XDG_DATA_HOME CC_USAGE_DATA_DIR XDG_CONFIG_HOME CLAUDE_CONFIG_DIR SUDO_USER 2>/dev/null || true

m() {
    # m <expected exit: 0|nonzero> <make args...>; output in $T/last.log
    want=$1
    shift
    # stdin is not a terminal, so nothing prompts for the alias.
    if "$MAKE" -C "$B" VERIFY_SKIP_MODES=$SKIP_MODES VERIFY_WAIT=6 "$@" >"$T/last.log" 2>&1 </dev/null; then rc=0; else rc=$?; fi
    if [ "$want" = 0 ] && [ "$rc" != 0 ]; then fail "make $* exited $rc"; fi
    if [ "$want" = nonzero ] && [ "$rc" = 0 ]; then fail "make $* should have failed"; fi
    return 0
}
has() { grep -qF -- "$1" "$T/last.log" || fail "output of the last make lacks: $1"; }
lacks() { if grep -qF -- "$1" "$T/last.log"; then fail "output of the last make has: $1"; fi; }

# ------------------------------------------------------------------ tests
m 0 help
has "cc-usage publisher bundle for 'sim'"
ok "make help lists the targets"

m 0
has "make check-prereqs"
ok "plain make shows help"

m 0 check-prereqs
has "PASS  checksums"
has "PASS  architecture"
lacks "FAIL"
ok "check-prereqs passes"

m 0 check-prereqs "DATA_DIR=$T/other"
has "WARN  data dir"
ok "check-prereqs warns when DATA_DIR is not where cc-hook writes"

# busybox's sha256sum (Alpine) has no --quiet: a stub that rejects long options.
mkdir -p "$T/busybox"
REAL_SHA=$(command -v sha256sum)
cat >"$T/busybox/sha256sum" <<EOS
#!/bin/sh
for a; do case \$a in --*) echo "sha256sum: unrecognized option '\$a'" >&2; exit 1 ;; esac; done
exec "$REAL_SHA" "\$@"
EOS
chmod +x "$T/busybox/sha256sum"
NO_BB_PATH=$PATH
PATH=$T/busybox:$PATH
m 0 check-prereqs
has "PASS  checksums"
PATH=$NO_BB_PATH
ok "check-prereqs works with busybox's sha256sum"

# Extracted with sudo, the files belong to root: seen from another uid here.
m 0 check-prereqs
has "PASS  bundle owner"
export SIM_UID=4242
m nonzero check-prereqs
has "FAIL  bundle owner"
has "sudo chown -R"
m nonzero install
has "FAIL  bundle owner"
[ ! -e "$P/bin/cc-publish" ] || fail "install went ahead with foreign-owned bundle files"
# Running make itself under sudo installs for root, not for the user.
export SIM_UID=0 SUDO_USER=alice
m nonzero check-prereqs
has "FAIL  user"
has "Run make as alice, without sudo"
# install.sh repeats the check when run without make (so without check-prereqs).
sh_direct() {
    # sh_direct <script> [VAR=value...]: runs scripts/<script> in the bundle
    # folder without make; output in $T/last.log, exit status returned.
    sd_script=$1
    shift
    (cd "$B" && env "$@" sh "scripts/$sd_script") >"$T/last.log" 2>&1 </dev/null
}
if sh_direct install.sh; then fail "install.sh ran under sudo"; fi
has "Run make as alice, without sudo"
unset SIM_UID SUDO_USER
export SIM_UID=4242
if sh_direct install.sh; then fail "install.sh ran with foreign-owned bundle files"; fi
has "sudo chown -R"
unset SIM_UID
[ ! -e "$P/bin/cc-publish" ] || fail "install.sh went ahead under sudo or with foreign-owned files"
if [ "$SKIP_MODES" = 0 ] && [ "$(id -u)" != 0 ]; then
    # Root-owned 0600 files cannot even be read: make stops before anything.
    chmod 000 "$B/config/publish.json"
    m nonzero help
    has "cannot read the bundle files"
    has "sudo chown -R"
    chmod 600 "$B/config/publish.json"
    # A world-writable bundle could have been tampered with by anyone.
    chmod o+w "$B/scripts/ctl.sh"
    m nonzero check-prereqs
    has "FAIL  bundle owner"
    has "world-writable"
    if sh_direct install.sh; then fail "install.sh ran with a world-writable bundle"; fi
    has "world-writable"
    chmod o-w "$B/scripts/ctl.sh"
fi
ok "check-prereqs and install refuse bundle files that are not the user's, and sudo"

# A newline in a path would end the unit's line (check_path).
if sh_direct install.sh "PREFIX=$T/new
line"; then fail "install.sh took a PREFIX with a newline"; fi
has "PREFIX contains a newline"
[ ! -e "$T/new" ] || fail "install.sh created a PREFIX with a newline"
ok "install refuses a path with a newline"

m 0 install
[ -x "$P/bin/cc-hook" ] && [ -x "$P/bin/cc-publish" ] && [ -x "$P/bin/cc-publish-autostart" ] || fail "binaries not installed"
[ -f "$P/nats/cc-sessions.pub-sim.creds" ] || fail "secrets not installed"
[ -f "$P/keys/publisher.key" ] && cmp -s "$P/keys/viewer.pub" "$B/config/viewer.pub" || fail "key pair files not installed"
first_kid=$(sed -n '1s/:.*//p' "$P/keys/publisher.key")
has "generated  this machine's identity $first_kid"
has "public key $first_kid:D"
has "accept this key when cc-usage asks about 'sim': key $first_kid, fingerprint DDDDDDDD"
grep -qF "\"creds\": \"$P/nats/cc-sessions.pub-sim.creds\"" "$P/publish.json" || fail "publish.json creds not absolute"
grep -qF "\"publisher_key\": \"$P/keys/publisher.key\"" "$P/publish.json" && grep -qF "\"viewer_pub\": \"$P/keys/viewer.pub\"" "$P/publish.json" ||
    fail "publish.json key files not absolute"
grep -qE '[ *]keys/publisher\.key$' "$P/install-manifest.sha256" && grep -qE '[ *]keys/viewer\.pub$' "$P/install-manifest.sha256" ||
    fail "the manifest lacks the key files"
grep -qxF "DATA_DIR=$P" "$P/publisher.env" || fail "publisher.env data dir"
if grep -qE '"(user|data_dir)"' "$P/publish.json"; then fail "publish.json has members cc-publish rejects"; fi
grep -qF '"servers": ["wss://connect.ngs.global", "tls://connect.ngs.global"]' "$P/publish.json" || fail "servers lost"
[ -f "$UNIT" ] || fail "unit not installed"
grep -qF "ExecStart=\"$P/bin/cc-publish\" run --config \"$P/publish.json\"" "$UNIT" || fail "ExecStart wrong"
grep -qF "Environment=\"CC_USAGE_DATA_DIR=$P\"" "$UNIT" || fail "unit data dir"
if grep -q 'CLAUDE_CONFIG_DIR=' "$UNIT" "$P/publisher.env"; then fail "default ~/.claude must not set CLAUDE_CONFIG_DIR"; fi
grep -qF "$T/claudebin" "$UNIT" || fail "unit PATH lacks the claude dir"
if grep -q '@[A-Z_]*@' "$UNIT"; then fail "unit has unrendered placeholders"; fi
grep -qF "$P/bin/cc-hook" "$H/.claude/settings.json" || fail "hooks not registered to the installed cc-hook"
[ -f "$SIM_STATE/enabled" ] && [ -f "$SIM_STATE/active" ] || fail "service not enabled and started"
[ -f "$P/bundle/Makefile" ] && [ -f "$P/bundle/config/installed-copy" ] || fail "tooling copy missing"
if [ -d "$P/bundle/bin" ] || [ -d "$P/bundle/secrets" ]; then fail "tooling copy holds binaries or secrets"; fi
has "loginctl enable-linger"
if [ "$SKIP_MODES" = 0 ]; then
    [ "$(stat -c %a "$P/nats/cc-sessions.pub-sim.creds")" = 600 ] && [ "$(stat -c %a "$P/nats")" = 700 ] && [ "$(stat -c %a "$P/bin/cc-publish")" = 755 ] || fail "modes"
    [ "$(stat -c %a "$P/keys")" = 700 ] && [ "$(stat -c %a "$P/keys/publisher.key")" = 600 ] || fail "key pair modes"
fi
ok "install puts everything in place"

cp "$UNIT" "$T/unit.first"
m 0 install
has "unchanged  $P/bin/cc-publish"
has "unchanged  $UNIT"
cmp -s "$UNIT" "$T/unit.first" || fail "second install changed the unit"
[ "$(grep -c 'restart cc-publish.service' "$SIM_STATE/calls.log")" = 2 ] || fail "second install did not restart"
ls "$P/nats/cc-sessions.pub-sim.creds".bak-* >/dev/null 2>&1 && fail "identical creds were backed up"
ok "install is idempotent"

# ------------------------------------------------------------------ identity
# The key pair is made by the first install (above) and kept by every later
# one.
K=$P/keys/publisher.key
cp "$K" "$T/identity.first"
m 0 install
has "kept       this machine's identity $first_kid ($K)"
lacks "generated  this machine's identity"
cmp -s "$K" "$T/identity.first" || fail "a reinstall replaced the key"
ok "a reinstall keeps the machine's identity"

# A lost key on an installed machine is never replaced silently: only with
# CONFIRM_ALIAS=<alias> on the command line (stdin is no terminal here, so
# there is no prompt), and nothing is installed without it.
mv "$K" "$T/identity.saved"
cp "$P/install-manifest.sha256" "$T/manifest.identity"
m nonzero install
has "$K is missing, but a publisher was installed in $P before"
has "This replaces sim's identity. The viewer will hold back its values until you accept the new key there."
has "make install CONFIRM_ALIAS=sim"
lacks "installing the cc-usage publisher"
[ ! -e "$K" ] || fail "install made a new key without the confirmation"
cmp -s "$P/install-manifest.sha256" "$T/manifest.identity" || fail "a refused install changed the manifest"
m nonzero install CONFIRM_ALIAS=other
has "not confirmed: 'other' is not this machine's alias 'sim'"
[ ! -e "$K" ] || fail "install made a new key with the wrong alias"
export CONFIRM_ALIAS=sim
m nonzero install
unset CONFIRM_ALIAS
[ ! -e "$K" ] || fail "an exported CONFIRM_ALIAS confirmed a new identity"
m 0 install CONFIRM_ALIAS=sim
has "generated  this machine's identity"
has "accept this key when cc-usage asks about 'sim'"
[ -f "$K" ] && ! cmp -s "$K" "$T/identity.saved" || fail "no new key after the confirmation"
m 0 verify
has "PASS  installed files"
lacks "FAIL"
ok "a lost key needs CONFIRM_ALIAS=<alias> on the command line for a new identity"

# confirm_identity (scripts/identity.sh, which lib.sh and update.sh both
# source) ignores whitespace around the given alias, like Install.ps1, and
# nothing else. Run by `sh`, the SIM_SH shell when one is given.
ci() {
    # ci <expected exit: 0|nonzero> <CONFIRM_ALIAS value>; output in $T/last.log
    if CONFIRM_ALIAS=$2 sh -c '. "$1"; confirm_identity sim' sh "$SRC/scripts/identity.sh" >"$T/last.log" 2>&1 </dev/null; then rc=0; else rc=$?; fi
    if [ "$1" = 0 ] && [ "$rc" != 0 ]; then fail "confirm_identity refused '$2'"; fi
    if [ "$1" = nonzero ] && [ "$rc" = 0 ]; then fail "confirm_identity accepted '$2'"; fi
    return 0
}
TAB=$(printf '\t')
NL='
'
ci 0 sim
ci 0 "  sim$TAB"
ci 0 "${NL}sim $NL"
has "This replaces sim's identity."
ci nonzero "s im"
has "not confirmed: 's im' is not this machine's alias 'sim'"
ci nonzero "   "
has "not confirmed: '' is not this machine's alias 'sim'"
ci nonzero "sim2"
ci nonzero ""
has "pass CONFIRM_ALIAS=sim"
ok "confirm_identity trims surrounding whitespace only"

# A key that does not load: the same, and a copy of it is kept.
echo "not a key" >"$K"
m nonzero install
has "does not load"
has "make install CONFIRM_ALIAS=sim"
[ "$(cat "$K")" = "not a key" ] || fail "install replaced a broken key without the confirmation"
m 0 install CONFIRM_ALIAS=sim
has "generated  this machine's identity"
ls "$K".bak-* >/dev/null 2>&1 || fail "the broken key was not backed up"
grep -qxE 'p[0-9]+:C{43}=' "$K" || fail "the broken key was not replaced"
rm -f "$K".bak-*
m 0 verify
lacks "FAIL"
ok "a key that does not load is replaced only when confirmed, with a backup"

# make rotate-key: the one way to change the identity on purpose.
cp "$K" "$T/identity.before"
kid_before=$(sed -n '1s/:.*//p' "$K")
m nonzero rotate-key
has "This replaces sim's identity. The viewer will hold back its values until you accept the new key there."
has "pass CONFIRM_ALIAS=sim"
cmp -s "$K" "$T/identity.before" || fail "rotate-key without the confirmation changed the key"
m nonzero rotate-key CONFIRM_ALIAS=other
cmp -s "$K" "$T/identity.before" || fail "rotate-key with the wrong alias changed the key"
restarts=$(grep -c 'restart cc-publish.service' "$SIM_STATE/calls.log")
m 0 rotate-key CONFIRM_ALIAS=sim
has "current identity $kid_before"
has "this machine's new public key: p"
has "accept this key when cc-usage asks about 'sim'"
[ "$(wc -l <"$K" | tr -d ' ')" = 2 ] && [ "$(sed -n 2p "$K")" = "$(cat "$T/identity.before")" ] ||
    fail "rotate-key did not put a new key in front of the old one"
[ "$(sed -n '1s/:.*//p' "$K")" != "$kid_before" ] || fail "rotate-key kept the key id"
[ "$(grep -c 'restart cc-publish.service' "$SIM_STATE/calls.log")" = $((restarts + 1)) ] || fail "rotate-key did not restart the publisher"
m 0 verify
has "PASS  installed files"
lacks "FAIL"
m 0 -C "$P/bundle" rotate-key CONFIRM_ALIAS=sim
[ "$(wc -l <"$K" | tr -d ' ')" = 3 ] || fail "rotate-key from the installed copy"
m 0 -C "$P/bundle" verify
lacks "FAIL"
ok "make rotate-key asks for the alias, rotates, restarts and keeps the manifest right"

export SIM_UID=0 SUDO_USER=alice
m nonzero uninstall
has "Run make as alice, without sudo"
unset SIM_UID SUDO_USER
[ -f "$UNIT" ] && [ -x "$P/bin/cc-publish" ] && [ -f "$SIM_STATE/active" ] || fail "uninstall under sudo touched the install"
ok "uninstall refuses sudo"

m 0 verify
has "PASS  installed files"
has "PASS  hooks"
has "PASS  status line chain"
has "PASS  cc-publish check"
has "PASS  service"
has "PASS  recent publish"
lacks "FAIL"
ok "verify passes after install"

PATH=$T/busybox:$PATH
m 0 verify
has "PASS  installed files"
lacks "FAIL"
PATH=$NO_BB_PATH
ok "verify works with busybox's sha256sum"

: >"$SIM_STATE/no-status-cmd"
m 0 verify
has "PASS  recent publish"
has "log: published host heartbeat"
rm -f "$SIM_STATE/no-status-cmd"
ok "verify falls back to the journal when cc-publish has no status command"

cp "$P/publish.json" "$T/publish.json.good"
sed 's/"alias": "sim"/"alias": "other"/' "$T/publish.json.good" >"$P/publish.json"
m nonzero verify
has "FAIL  installed files"
has "FAIL  publish.json"
cp "$T/publish.json.good" "$P/publish.json"
ok "verify fails on a modified install"

rm -f "$SIM_STATE/active"
m nonzero verify
has "FAIL  service"
m 0 restart
m 0 verify
ok "verify fails while the service is down, passes after make restart"

m 0 status
has "cc-hook status"
ok "make status"

m 0 -C "$P/bundle" verify
lacks "FAIL"
m nonzero -C "$P/bundle" install
has "installed copy"
ok "the installed tooling copy verifies and refuses to install"

# Hooks lost (or an event list that changed): verify points to make hooks,
# which works from the installed copy too.
cp "$H/.claude/settings.json" "$T/settings.hooked"
echo '{}' >"$H/.claude/settings.json"
m nonzero -C "$P/bundle" verify
has "FAIL  hooks"
has "run make hooks"
m 0 -C "$P/bundle" hooks
has "registering hooks and the status line chain"
grep -qF "$P/bin/cc-hook" "$H/.claude/settings.json" || fail "make hooks did not register the hooks"
m 0 -C "$P/bundle" verify
has "PASS  hooks"
lacks "FAIL"
m 0 hooks
cmp -s "$H/.claude/settings.json" "$T/settings.hooked" || fail "make hooks registered something else than make install"
ok "make hooks registers the hooks again, from the bundle and from the installed copy"

printf -- '-----BEGIN NATS USER JWT-----\nother\n------END NATS USER JWT------\n' >"$P/nats/cc-sessions.pub-sim.creds"
m 0 install
ls "$P/nats/cc-sessions.pub-sim.creds".bak-* >/dev/null 2>&1 || fail "changed creds were not backed up"
cmp -s "$P/nats/cc-sessions.pub-sim.creds" "$B/secrets/cc-sessions.pub-sim.creds" || fail "creds not replaced"
ok "install backs up different existing creds"

# ------------------------------------------------------------------ update.sh
# An update package (make-publisher-bundle.ps1 -Update): new binaries and
# update.sh at its root, with SHA256SUMS; no secrets, no scripts/lib.sh.
U="$T/update pkg/cc-usage-publisher-update-sim"
mkdir -p "$U/bin" "$U/bundle/scripts" "$U/bundle/systemd"
sed 's/ 0\.1\.0"/ 0.1.1"/' "$T/cc-hook" >"$U/bin/cc-hook"
sed 's/ 0\.1\.0"/ 0.1.1"/' "$T/cc-publish" >"$U/bin/cc-publish"
cp "$SRC/scripts/update.sh" "$U/update.sh"
# The viewer's public key, the same as the bundle's, and the tooling.
cp "$B/config/viewer.pub" "$U/viewer.pub"
cp "$SRC/Makefile" "$SRC/README.md" "$U/bundle/"
cp "$SRC"/scripts/*.sh "$U/bundle/scripts/"
cp "$SRC/systemd/cc-publish.service.in" "$U/bundle/systemd/"
chmod +x "$U/bin/cc-hook" "$U/bin/cc-publish"
pkg_sums() {
    (cd "$U" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | sort | while read -r f; do sha256sum "$f"; done) >"$T/SHA256SUMS.tmp"
    mv "$T/SHA256SUMS.tmp" "$U/SHA256SUMS"
}
pkg_sums
# update.sh runs plain `make`; this one passes what m() passes.
mkdir -p "$T/makebin"
MAKE_PATH=$(command -v "$MAKE")
printf '#!/bin/sh\nexec "%s" "$@" VERIFY_SKIP_MODES=%s VERIFY_WAIT=6\n' "$MAKE_PATH" "$SKIP_MODES" >"$T/makebin/make"
chmod +x "$T/makebin/make"
u() {
    # u <expected exit: 0|nonzero> [update.sh args...]; output in $T/last.log
    want=$1
    shift
    if PATH="$T/makebin:$PATH" sh "$U/update.sh" "$@" >"$T/last.log" 2>&1 </dev/null; then rc=0; else rc=$?; fi
    if [ "$want" = 0 ] && [ "$rc" != 0 ]; then fail "update.sh $* exited $rc"; fi
    if [ "$want" = nonzero ] && [ "$rc" = 0 ]; then fail "update.sh $* should have failed"; fi
    return 0
}
# Manifest lines without the two binaries, and each line's number and path.
not_bin() { grep -v -e '[ *]bin/cc-hook$' -e '[ *]bin/cc-publish$' -e '[ *]bin/cc-publish-autostart$' "$1" || true; }
layout() { awk '{ print NR, $2 }' "$1"; }

export SIM_UID=0 SUDO_USER=alice
u nonzero
has "Run sh update.sh as alice, without sudo"
unset SIM_UID SUDO_USER
cmp -s "$P/bin/cc-publish" "$B/bin/cc-publish" || fail "update.sh under sudo replaced a binary"
export SIM_UID=4242
u nonzero
has "files that are not yours"
unset SIM_UID
ok "update.sh refuses sudo and a package that is not the user's"

cp "$P/install-manifest.sha256" "$T/manifest.before"
not_bin "$T/manifest.before" >"$T/others.before"
[ -s "$T/others.before" ] || fail "the manifest has no entries besides the binaries"
# A newer launcher in the package, hooks that the new cc-hook's event list
# no longer matches (none registered here), and an exported PREFIX (Termux)
# that update.sh must ignore.
printf '# launcher of 0.1.1\n' >>"$U/bundle/scripts/cc-publish-autostart.sh"
pkg_sums
echo '{}' >"$H/.claude/settings.json"
PATH=$T/busybox:$PATH
export PREFIX="$T/termux prefix"
u 0
unset PREFIX
PATH=$NO_BB_PATH
has "updated $P/bin: cc-publish 0.1.0 -> cc-publish 0.1.1"
has "registering hooks and the status line chain"
has "PASS  installed files"
has "PASS  hooks"
lacks "FAIL"
cmp -s "$P/bin/cc-hook" "$U/bin/cc-hook" && cmp -s "$P/bin/cc-publish" "$U/bin/cc-publish" || fail "binaries not updated"
cmp -s "$P/bin/cc-publish-autostart" "$U/bundle/scripts/cc-publish-autostart.sh" || fail "launcher not updated"
[ -x "$P/bin/cc-publish-autostart" ] || fail "updated launcher is not executable"
grep -qF "$P/bin/cc-hook" "$H/.claude/settings.json" || fail "update.sh did not register the hooks again"
[ ! -e "$T/termux prefix" ] || fail "update.sh used an exported PREFIX"
not_bin "$P/install-manifest.sha256" >"$T/others.after"
cmp -s "$T/others.before" "$T/others.after" || fail "update.sh changed manifest lines other than the binaries'"
[ "$(layout "$T/manifest.before")" = "$(layout "$P/install-manifest.sha256")" ] || fail "update.sh moved, added or dropped manifest lines"
(cd "$U" && sha256sum bin/cc-hook bin/cc-publish) >"$T/bin.sums"
(cd "$U/bundle/scripts" && sha256sum cc-publish-autostart.sh) | sed 's|\([ *]\)cc-publish-autostart\.sh$|\1bin/cc-publish-autostart|' >>"$T/bin.sums"
grep -e '[ *]bin/cc-hook$' -e '[ *]bin/cc-publish$' -e '[ *]bin/cc-publish-autostart$' "$P/install-manifest.sha256" >"$T/bin.after"
cmp -s "$T/bin.sums" "$T/bin.after" || fail "manifest lines of the binaries and the launcher not recomputed"
if [ "$SKIP_MODES" = 0 ]; then
    [ "$(stat -c %a "$P/install-manifest.sha256")" = 600 ] || fail "manifest mode after update"
fi
ls "$P"/.install-manifest* >/dev/null 2>&1 && fail "update.sh left temporary files"
has "kept this machine's identity $(sed -n '1s/:.*//p' "$P/keys/publisher.key")"
ok "update.sh replaces the binaries and the launcher, registers the hooks again, recomputes only their manifest lines and ignores an exported PREFIX (busybox sha256sum)"

# A lost key: update.sh changes nothing unless CONFIRM_ALIAS=<alias> is given.
cp "$B/bin/cc-publish" "$P/bin/cc-publish"
cp "$P/install-manifest.sha256" "$T/manifest.lost"
mv "$P/keys/publisher.key" "$T/identity.update"
u nonzero
has "$P/keys/publisher.key is missing"
has "This replaces sim's identity."
has "sh update.sh CONFIRM_ALIAS=sim"
lacks "updated $P/bin"
cmp -s "$P/bin/cc-publish" "$B/bin/cc-publish" || fail "update.sh replaced a binary without an identity"
cmp -s "$P/install-manifest.sha256" "$T/manifest.lost" || fail "a refused update.sh changed the manifest"
u nonzero CONFIRM_ALIAS=other
has "not confirmed: 'other'"
export CONFIRM_ALIAS=sim
u nonzero
unset CONFIRM_ALIAS
[ ! -e "$P/keys/publisher.key" ] || fail "update.sh made a key without the confirmation"
# Whitespace around the alias is ignored (identity.sh, checked above).
u 0 "CONFIRM_ALIAS= sim "
has "generated this machine's identity"
has "accept this key when cc-usage asks about 'sim'"
has "PASS  installed files"
lacks "FAIL"
[ -f "$P/keys/publisher.key" ] || fail "update.sh did not make the confirmed key"
ok "update.sh makes a new identity for a lost key only with CONFIRM_ALIAS=<alias> (surrounding whitespace ignored)"

# Any other installed file that changed since make install stops the update
# before anything is replaced, so make verify still reports it.
cp "$B/bin/cc-publish" "$P/bin/cc-publish"
cp "$P/install-manifest.sha256" "$T/manifest.mid"
cp "$P/publisher.env" "$T/publisher.env.good"
echo "EXTRA=1" >>"$P/publisher.env"
u nonzero
has "installed files changed since make install"
has "publisher.env"
lacks "updated $P/bin"
cmp -s "$P/bin/cc-publish" "$B/bin/cc-publish" || fail "update.sh replaced a binary despite a changed file"
cmp -s "$P/install-manifest.sha256" "$T/manifest.mid" || fail "update.sh rewrote the manifest despite a changed file"
cp "$T/publisher.env.good" "$P/publisher.env"
u 0
cmp -s "$P/bin/cc-publish" "$U/bin/cc-publish" || fail "update.sh did not update after the file was restored"
lacks "FAIL"
ok "update.sh aborts when another installed file no longer matches the manifest"

# A package with another viewer key: update.sh installs it where
# publish.json says and recomputes its manifest line, refreshes the tooling
# copy, and a second run changes nothing.
m 0 install
printf '# stale copy\n' >>"$P/bundle/scripts/verify.sh"
cp "$U/viewer.pub" "$T/viewer.pub.pkg"
sed 's/F/G/g; s/^k1:/k2:/' "$T/viewer.pub.pkg" >"$U/viewer.pub"
pkg_sums
u 0
has "installed $P/keys/viewer.pub"
has "kept this machine's identity"
has "PASS  installed files"
lacks "FAIL"
cmp -s "$P/keys/viewer.pub" "$U/viewer.pub" || fail "update.sh did not install the new viewer.pub"
(cd "$P" && sha256sum -c install-manifest.sha256 >/dev/null) || fail "the manifest does not match after update.sh"
[ -f "$P/bundle/scripts/update.sh" ] && cmp -s "$P/bundle/scripts/verify.sh" "$SRC/scripts/verify.sh" || fail "tooling copy not refreshed"
cp "$P/keys/publisher.key" "$T/publisher.key.made"
cp "$P/install-manifest.sha256" "$T/manifest.again"
u 0
has "unchanged $P/keys/viewer.pub"
cmp -s "$P/keys/publisher.key" "$T/publisher.key.made" || fail "a second update.sh replaced the key pair"
cmp -s "$P/install-manifest.sha256" "$T/manifest.again" || fail "a second update.sh changed the manifest"
cp "$T/viewer.pub.pkg" "$U/viewer.pub"
pkg_sums
ok "update.sh installs a new viewer key, refreshes the tooling copy and is idempotent"

# publish.json must name both key files (make install writes them).
cp "$P/publish.json" "$T/publish.json.keys"
cp "$P/install-manifest.sha256" "$T/manifest.keys"
grep -v '"viewer_pub"' "$T/publish.json.keys" >"$P/publish.json"
cp "$P/bin/cc-publish" "$T/cc-publish.installed"
u nonzero
has "does not name both key files"
cmp -s "$P/bin/cc-publish" "$T/cc-publish.installed" || fail "update.sh replaced a binary without the key files named"
cp "$T/publish.json.keys" "$P/publish.json"
cmp -s "$P/install-manifest.sha256" "$T/manifest.keys" || fail "a refused update.sh changed the manifest"
ok "update.sh refuses a publish.json that does not name both key files"

u nonzero "PREFIX=relative/dir"
has "PREFIX must be an absolute path"
u nonzero "PREFIX=$T/nothing here"
has "no cc-usage publisher installed"
ok "update.sh refuses a relative or empty PREFIX"

# Back to the bundle's binaries for the tests below.
m 0 install

m 0 uninstall
[ ! -f "$UNIT" ] || fail "unit left behind"
[ ! -e "$P/bin/cc-publish" ] || fail "binary left behind"
[ ! -d "$P/bundle" ] || fail "tooling copy left behind"
[ -f "$P/keys/publisher.key" ] && [ -f "$P/nats/cc-sessions.pub-sim.creds" ] || fail "secrets removed without PURGE"
[ "$(cat "$H/.claude/settings.json")" = '{}' ] || fail "hooks not removed"
[ ! -f "$SIM_STATE/enabled" ] && [ ! -f "$SIM_STATE/active" ] || fail "service not disabled"
m 0 uninstall
ok "uninstall removes everything but the secrets, and runs twice"

# Only the command line sets the Makefile's variables: Termux and build
# tools export PREFIX, and an exported PURGE=1 must not destroy the secrets.
export PREFIX="$T/elsewhere" DATA_DIR="$T/elsewhere" PURGE=1
m 0 install
[ -x "$P/bin/cc-publish" ] || fail "an exported PREFIX moved the install"
[ ! -e "$T/elsewhere" ] || fail "an exported PREFIX or DATA_DIR was used"
m 0 uninstall
[ -f "$P/keys/publisher.key" ] && [ -f "$P/nats/cc-sessions.pub-sim.creds" ] || fail "an exported PURGE=1 purged the secrets"
unset PREFIX DATA_DIR PURGE
ok "exported PREFIX, DATA_DIR and PURGE are ignored"

# A custom PREFIX is remembered by the installed tooling copy, so verify and
# uninstall from it act on that install (the bundle folder is gone by then).
Q="$T/custom prefix"
m 0 install "PREFIX=$Q"
[ -x "$Q/bin/cc-publish" ] && [ -f "$Q/keys/publisher.key" ] || fail "custom PREFIX not installed"
grep -qF "$Q/bin/cc-hook" "$H/.claude/settings.json" || fail "hooks not registered to the custom PREFIX"
m 0 -C "$Q/bundle" verify
has "PASS  installed files"
lacks "FAIL"
# The stub cc-publish takes no lock; the real one leaves cc-publish.lock in
# the data dir (and in a PREFIX it publishes from).
: >"$P/cc-publish.lock"
: >"$Q/cc-publish.lock"
m 0 -C "$Q/bundle" uninstall PURGE=1
[ ! -e "$Q/bin/cc-publish" ] && [ ! -e "$Q/publish.json" ] && [ ! -e "$Q/keys" ] || fail "uninstall from the copy left the custom PREFIX install"
[ ! -e "$P/publish-state.json" ] || fail "PURGE left the publisher state in the data dir"
[ ! -e "$P/cc-publish.lock" ] && [ ! -e "$Q/cc-publish.lock" ] || fail "uninstall left the publisher's lock file"
[ "$(cat "$H/.claude/settings.json")" = '{}' ] || fail "hooks of the custom PREFIX not removed"
[ ! -f "$UNIT" ] || fail "unit left behind"
ok "the installed copy remembers a custom PREFIX for verify and uninstall"

# A custom Claude Code folder reaches the publisher as CLAUDE_CONFIG_DIR
# (the unit and publisher.env), and its settings.json gets the hooks.
C="$T/claude alt"
mkdir -p "$C"
m 0 install "CLAUDE_DIR=$C"
has "WARN  Claude Code folder"
grep -qxF "Environment=\"CLAUDE_CONFIG_DIR=$C\"" "$UNIT" || fail "unit lacks CLAUDE_CONFIG_DIR=$C"
grep -qxF "CLAUDE_CONFIG_DIR=$C" "$P/publisher.env" || fail "publisher.env lacks CLAUDE_CONFIG_DIR"
grep -qF "$P/bin/cc-hook" "$C/settings.json" || fail "hooks not registered in the custom settings.json"
m nonzero verify
has "pass the CLAUDE_DIR make install had"
m 0 verify "CLAUDE_DIR=$C"
has "CLAUDE_CONFIG_DIR $C"
lacks "FAIL"
[ "$(cat "$SIM_STATE/check.claude")" = "$C" ] || fail "cc-publish check ran without CLAUDE_CONFIG_DIR=$C"
m 0 -C "$P/bundle" verify
lacks "FAIL"
sed "/CLAUDE_CONFIG_DIR/d" "$UNIT" >"$T/unit.noclaude" && cp "$T/unit.noclaude" "$UNIT"
m nonzero -C "$P/bundle" verify
has "the unit does not set CLAUDE_CONFIG_DIR"
m 0 -C "$P/bundle" uninstall
[ "$(cat "$C/settings.json")" = '{}' ] || fail "hooks not removed from the custom settings.json"
ok "a custom CLAUDE_DIR is passed to the publisher as CLAUDE_CONFIG_DIR"

# CLAUDE_CONFIG_DIR in the environment is Claude Code's own setting: it is
# the default CLAUDE_DIR, and then nothing needs a warning.
export CLAUDE_CONFIG_DIR="$C"
m 0 install
lacks "WARN  Claude Code folder"
grep -qxF "Environment=\"CLAUDE_CONFIG_DIR=$C\"" "$UNIT" || fail "CLAUDE_CONFIG_DIR from the environment not used"
m 0 uninstall
[ "$(cat "$C/settings.json")" = '{}' ] || fail "hooks not removed from CLAUDE_CONFIG_DIR's settings.json"
unset CLAUDE_CONFIG_DIR
ok "CLAUDE_CONFIG_DIR from the environment is the default CLAUDE_DIR"

# An explicit CLAUDE_DIR=~/.claude wins over CLAUDE_CONFIG_DIR in the
# environment: cc-hook, whose default follows CLAUDE_CONFIG_DIR, runs without it.
export CLAUDE_CONFIG_DIR="$C"
rm -f "$C/settings.json"
m 0 install "CLAUDE_DIR=$H/.claude"
grep -qF "$P/bin/cc-hook" "$H/.claude/settings.json" || fail "hooks not registered in ~/.claude"
[ ! -f "$C/settings.json" ] || fail "CLAUDE_CONFIG_DIR in the environment redirected cc-hook"
m 0 uninstall "CLAUDE_DIR=$H/.claude"
[ "$(cat "$H/.claude/settings.json")" = '{}' ] || fail "hooks not removed from ~/.claude"
[ ! -f "$C/settings.json" ] || fail "uninstall touched CLAUDE_CONFIG_DIR's settings.json"
unset CLAUDE_CONFIG_DIR
ok "an explicit CLAUDE_DIR=~/.claude is not redirected by CLAUDE_CONFIG_DIR"

# systemd reads user units from $XDG_CONFIG_HOME/systemd/user when it is set.
X="$T/xdg config"
export XDG_CONFIG_HOME="$X"
m 0 install
[ -f "$X/systemd/user/cc-publish.service" ] || fail "unit not in \$XDG_CONFIG_HOME/systemd/user"
[ ! -e "$UNIT" ] || fail "unit written to ~/.config although XDG_CONFIG_HOME is set"
grep -qxF "UNIT_DIR=$X/systemd/user" "$P/bundle/config/installed-copy" || fail "installed copy does not remember UNIT_DIR"
m 0 verify
has "PASS  service"
lacks "FAIL"
m 0 uninstall
[ ! -e "$X/systemd/user/cc-publish.service" ] || fail "uninstall left the XDG unit"
unset XDG_CONFIG_HOME
ok "the unit goes to \$XDG_CONFIG_HOME/systemd/user"

# No systemd: install refuses, autostart-profile works.
: >"$SIM_STATE/no-systemd"
m 0 check-prereqs
has "WARN  systemd --user"
has "make autostart-profile"
m nonzero install
has "make autostart-profile"
OLD_SHELL=${SHELL:-}
export SHELL=/usr/bin/zsh
m 0 autostart-profile
export SHELL="$OLD_SHELL"
has "zsh, which does not read ~/.profile"
grep -qF '# >>> cc-usage publisher >>>' "$H/.profile" || fail "profile block missing"
grep -qF 'export FOO=1' "$H/.profile" || fail "profile content lost"
[ -f "$H/.profile.cc-usage.bak" ] || fail "profile not backed up"
sleep 2
[ -f "$P/cc-publish.pid" ] && kill -0 "$(cat "$P/cc-publish.pid")" 2>/dev/null || fail "publisher not running"
pid1=$(cat "$P/cc-publish.pid")
m 0 autostart-profile
[ "$(grep -c '# >>> cc-usage publisher >>>' "$H/.profile")" = 1 ] || fail "profile block duplicated"
# A new login shell runs the block; it must not start a second publisher.
pid2=$(cat "$P/cc-publish.pid")
(. "$H/.profile")
[ "$(cat "$P/cc-publish.pid")" = "$pid2" ] || fail "profile started a second publisher"
[ "$pid1" != "$pid2" ] || fail "reinstall did not restart the publisher"
m 0 verify
has "PASS  autostart"
has "PASS  recent publish"
lacks "FAIL"
m 0 restart
m 0 uninstall PURGE=1
if grep -qF 'cc-usage publisher' "$H/.profile"; then fail "profile block left behind"; fi
grep -qF 'export FOO=1' "$H/.profile" || fail "profile content lost on uninstall"
sleep 1
kill -0 "$pid2" 2>/dev/null && fail "publisher still running"
[ ! -e "$P/nats" ] && [ ! -e "$P/keys" ] || fail "PURGE left secrets"
ok "autostart-profile installs, verifies, restarts and uninstalls with PURGE=1"

rm -f "$SIM_STATE/run.claude"
m 0 autostart-profile "CLAUDE_DIR=$C"
sleep 2
[ "$(cat "$SIM_STATE/run.claude" 2>/dev/null)" = "$C" ] || fail "the ~/.profile launcher did not pass CLAUDE_CONFIG_DIR=$C"
m 0 verify "CLAUDE_DIR=$C"
has "PASS  autostart"
lacks "FAIL"
m 0 uninstall PURGE=1 "CLAUDE_DIR=$C"
[ "$(cat "$C/settings.json")" = '{}' ] || fail "hooks not removed from the custom settings.json"
ok "the ~/.profile launcher passes a custom CLAUDE_DIR as CLAUDE_CONFIG_DIR"
rm -f "$SIM_STATE/no-systemd"

# Broken bundles fail early.
cp "$B/bin/cc-publish" "$T/cc-publish.orig"
echo "# tampered" >>"$B/bin/cc-publish"
m nonzero install
has "FAIL  checksums"
[ ! -e "$P/bin/cc-publish" ] || fail "install went ahead with a bad checksum"
cp "$T/cc-publish.orig" "$B/bin/cc-publish"
export SIM_ARCH=aarch64
m nonzero check-prereqs
export SIM_ARCH=x86_64
has "FAIL  architecture"
mv "$H/.claude" "$H/.claude.off"
m nonzero check-prereqs
has "FAIL  Claude Code data"
mv "$H/.claude.off" "$H/.claude"
ok "check-prereqs fails on checksum, architecture and missing ~/.claude"

(cd "$T" && "$MAKE" -f "$B/Makefile" help >"$T/last.log" 2>&1) && fail "make outside the bundle should fail"
has "run make inside the extracted bundle folder"
ok "make outside the bundle folder stops with a clear message"

echo "all $pass simulation checks passed"
