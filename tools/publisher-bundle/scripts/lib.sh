# Shared helpers for the cc-usage publisher bundle scripts.
# POSIX sh (dash, busybox ash, bash): no arrays, no [[ ]], no local.
# Sourced by the other scripts, which the Makefile runs from the bundle folder.
# Never prints secrets: creds and key files are only copied, hashed and stat'ed.
# Variables set here are used by the scripts that source this file.
# shellcheck disable=SC2034

UNIT_NAME=cc-publish.service
MARK_BEGIN='# >>> cc-usage publisher >>>'
MARK_END='# <<< cc-usage publisher <<<'

step() { printf '==> %s\n' "$*"; }
say() { printf '%s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Value of a "key": "string" member in a one-member-per-line JSON file.
json_get() {
    sed -n 's/^[[:space:]]*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*$/\1/p' "$2" 2>/dev/null | head -n 1
}

# Value of KEY=value in a KEY=value file (never sourced, so never executed).
env_get() {
    sed -n 's/^'"$1"'=//p' "$2" 2>/dev/null | head -n 1
}

# Defaults, so the scripts also work when run without make. The Makefile
# computes the same values and exports them.
: "${HOME:?HOME is not set}"
# HOOK_DATA_DIR: where cc-hook writes the spool when Claude Code runs with
# this shell's environment (cc-core's data dir rule: an empty or relative
# XDG_DATA_HOME is skipped). DATA_DIR should match.
if [ -n "${CC_USAGE_DATA_DIR:-}" ]; then HOOK_DATA_DIR=$CC_USAGE_DATA_DIR
else
    case ${XDG_DATA_HOME:-} in
        /*) HOOK_DATA_DIR=$XDG_DATA_HOME/cc-usage ;;
        *) HOOK_DATA_DIR=$HOME/.local/share/cc-usage ;;
    esac
fi
: "${DATA_DIR:=$HOOK_DATA_DIR}"
: "${PREFIX:=$DATA_DIR}"
: "${UNIT_DIR:=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
: "${PROFILE:=$HOME/.profile}"
: "${CLAUDE_DIR:=${CLAUDE_CONFIG_DIR:-$HOME/.claude}}"
# Without a trailing slash, so ~/.claude/ is recognised as the default.
while :; do case $CLAUDE_DIR in ?*/) CLAUDE_DIR=${CLAUDE_DIR%/} ;; *) break ;; esac; done
: "${AUTOSTART:=systemd}"
: "${PURGE:=0}"
: "${LINGER:=0}"
: "${FORCE:=0}"
: "${VERIFY_WAIT:=45}"
: "${VERIFY_SKIP_MODES:=0}"
: "${CONFIRM_ALIAS:=}"

BUNDLE_DIR=$(pwd)
if [ ! -f "$BUNDLE_DIR/config/publish.json" ] || [ ! -f "$BUNDLE_DIR/config/bundle.env" ]; then
    die "run this from the bundle folder (make -C <folder> <target>); config/publish.json not found in $BUNDLE_DIR"
fi

ALIAS=$(json_get alias "$BUNDLE_DIR/config/publish.json")
NATS_USER=$(env_get NATS_USER "$BUNDLE_DIR/config/bundle.env")
ARCH=$(env_get ARCH "$BUNDLE_DIR/config/bundle.env")
VERSION=$(env_get VERSION "$BUNDLE_DIR/config/bundle.env")
[ -n "$ALIAS" ] || die "config/publish.json has no alias"
[ -n "$NATS_USER" ] || NATS_USER=cc-sessions.pub-$ALIAS
case $ALIAS in
    *[!a-z0-9-]* | -*) die "invalid alias '$ALIAS' in config/publish.json" ;;
esac

# The copy install.sh leaves in $PREFIX/bundle has the tooling but neither
# binaries nor secrets: it can verify, show status and uninstall.
INSTALLED_COPY=0
[ -f "$BUNDLE_DIR/config/installed-copy" ] && INSTALLED_COPY=1

UNIT_FILE=$UNIT_DIR/$UNIT_NAME
# KEY=value file install.sh writes next to publish.json: the data dir the
# hooks write to, read by the ~/.profile launcher and by make verify.
PUBLISHER_ENV=$PREFIX/publisher.env
CREDS_NAME=$NATS_USER.creds
LAUNCHER=$PREFIX/bin/cc-publish-autostart

# CLAUDE_CONFIG_DIR the publisher runs with: empty for the default folder
# (~/.claude), else CLAUDE_DIR. Not set for the default, because Claude Code
# with CLAUDE_CONFIG_DIR also reads its .claude.json from that folder
# instead of from $HOME.
if [ "$CLAUDE_DIR" = "$HOME/.claude" ]; then PUB_CLAUDE_CONFIG_DIR=
else PUB_CLAUDE_CONFIG_DIR=$CLAUDE_DIR
fi

# Runs cc-hook ($1) with a subcommand ($2) and its other arguments on
# settings.json in CLAUDE_DIR. cc-hook's default settings file follows
# CLAUDE_CONFIG_DIR, so it runs with that cleared and this shell's value
# never redirects it: for ~/.claude without --settings (the status line
# command stays plain), for any other folder with --settings (which the
# status line command then names too, so it works whatever environment
# Claude Code runs with).
hook_settings() {
    _hook=$1
    _sub=$2
    shift 2
    if [ "$CLAUDE_DIR" = "$HOME/.claude" ]; then
        (unset CLAUDE_CONFIG_DIR; "$_hook" "$_sub" "$@")
    else
        (unset CLAUDE_CONFIG_DIR; "$_hook" "$_sub" --settings "$CLAUDE_DIR/settings.json" "$@")
    fi
}

# Exports the publisher's environment: the data dir the hooks write to and,
# when it is not ~/.claude, Claude Code's folder. Call it in a subshell, e.g.
# out=$(pub_exports; cc-publish status ...), so nothing leaks into the caller.
pub_exports() {
    CC_USAGE_DATA_DIR=$DATA_DIR
    export CC_USAGE_DATA_DIR
    if [ -n "$PUB_CLAUDE_CONFIG_DIR" ]; then
        CLAUDE_CONFIG_DIR=$PUB_CLAUDE_CONFIG_DIR
        export CLAUDE_CONFIG_DIR
    fi
}

# Paths go into a systemd unit and a shell profile; spaces are fine, quoting
# characters are not, and neither is a newline (it would end the unit line,
# and KEY=value files are read line by line).
NL='
'
check_path() {
    case $2 in
        *[\"\'\\$%\`]*) die "$1 contains a quote, backslash, '\$', '%' or backquote, which the unit file and ~/.profile cannot carry: $2" ;;
        *"$NL"*) die "$1 contains a newline, which the unit file and ~/.profile cannot carry: $2" ;;
    esac
    case $2 in
        /*) ;;
        *) die "$1 must be an absolute path: $2" ;;
    esac
}

# sudo_problem: under sudo, HOME, ~/.claude and the systemd user manager are
# root's, so everything would be done for root instead of the user who runs
# Claude Code. Prints why and returns 1 under sudo; plain root (a machine
# where root runs Claude Code) passes, check-prereqs only warns about it.
# update.sh, which runs without this file, has a copy of this check.
sudo_problem() {
    if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
        printf 'running under sudo: this would act for root, not for %s. Run make as %s, without sudo\n' "$SUDO_USER" "$SUDO_USER"
        return 1
    fi
    return 0
}

# bundle_problem: the bundle folder (the current directory) must be the
# user's and not world-writable, or someone else could have swapped its
# binaries, scripts and SHA256SUMS. The archive stores owner 0 without names,
# so a plain tar gives the files to whoever extracts them; extracted with
# sudo they belong to root. Prints why and returns 1 on a problem.
# update.sh has a copy of this check.
bundle_problem() {
    bp_uid=$(id -u)
    bp_bad=$(find . ! -user "$bp_uid" -print 2>/dev/null | head -n 3 | tr '\n' ' ')
    if [ -n "$bp_bad" ]; then
        printf 'files that are not yours (extracted with sudo?): %s- fix: sudo chown -R "%s:" "%s"\n' "$bp_bad" "$(id -un)" "$BUNDLE_DIR"
        return 1
    fi
    bp_bad=$(find . ! -type l -perm -0002 -print 2>/dev/null | head -n 3 | tr '\n' ' ')
    if [ -n "$bp_bad" ]; then
        printf 'world-writable files or folders (anyone could have replaced them): %s- fix: chmod -R go-w "%s", or better, extract the bundle again\n' "$bp_bad" "$BUNDLE_DIR"
        return 1
    fi
    return 0
}

# refuse_unsafe_run: the sudo and bundle folder checks of check-prereqs,
# repeated by install.sh and uninstall.sh because they can be run without
# make (sh scripts/install.sh) and so without check-prereqs.
refuse_unsafe_run() {
    if ! rur_why=$(sudo_problem); then die "$rur_why"; fi
    if ! rur_why=$(bundle_problem); then die "$rur_why"; fi
}

# Octal permission bits of a file, e.g. 600.
file_mode() {
    stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1" 2>/dev/null
}

# First four bytes and e_machine of an ELF file, as hex: "7f454c46 3e00".
elf_magic() { od -An -t x1 -N 4 "$1" 2>/dev/null | tr -d ' \n'; }
elf_machine() { od -An -t x1 -j 18 -N 2 "$1" 2>/dev/null | tr -d ' \n'; }
machine_arch() {
    case $1 in
        3e00) echo x86_64 ;;
        b700) echo aarch64 ;;
        *) echo "unknown($1)" ;;
    esac
}
host_arch() {
    case $(uname -m) in
        x86_64 | amd64) echo x86_64 ;;
        aarch64 | arm64) echo aarch64 ;;
        *) uname -m ;;
    esac
}

is_wsl() { grep -qi microsoft /proc/version 2>/dev/null; }

# Whether `systemctl --user` reaches a running user manager.
systemd_user_ok() {
    have systemctl && systemctl --user show-environment >/dev/null 2>&1
}

systemd_why_not() {
    if ! have systemctl; then
        echo "systemctl is not installed"
    elif [ ! -d /run/systemd/system ]; then
        if is_wsl; then
            echo "this WSL distribution runs without systemd (add [boot] systemd=true to /etc/wsl.conf and run 'wsl --shutdown', or use the ~/.profile fallback)"
        else
            echo "this system was not booted with systemd (a container or a non-systemd init)"
        fi
    else
        echo "systemd runs but the user manager is not reachable from this shell (no XDG_RUNTIME_DIR or D-Bus session; log in directly or over ssh instead of su/sudo)"
    fi
}

# How the publisher is started on this machine: systemd, profile or none.
installed_mode() {
    if [ -f "$UNIT_FILE" ]; then echo systemd
    elif [ -f "$PROFILE" ] && grep -qF "$MARK_BEGIN" "$PROFILE" 2>/dev/null; then echo profile
    else echo none
    fi
}

# render TEMPLATE: prints TEMPLATE with @NAME@ replaced by $R_NAME for the
# names listed in $RENDER_VARS. Plain string replacement, no regex.
render() {
    awk '
    function repl(s, k, v,    out, i) {
        out = ""
        while ((i = index(s, k)) > 0) { out = out substr(s, 1, i - 1) v; s = substr(s, i + length(k)) }
        return out s
    }
    BEGIN { n = split(ENVIRON["RENDER_VARS"], names, " "); for (j = 1; j <= n; j++) vals[names[j]] = ENVIRON["R_" names[j]] }
    { line = $0; for (j = 1; j <= n; j++) line = repl(line, "@" names[j] "@", vals[names[j]]); print line }
    ' "$1"
}

# put_file SRC DEST MODE: copies atomically (temp file and rename, so a
# running binary is never overwritten in place) and sets MODE. With BACKUP=1
# an existing different DEST is kept as DEST.bak-<timestamp>.
put_file() {
    if [ -f "$2" ] && cmp -s "$1" "$2"; then
        chmod "$3" "$2"
        say "  unchanged  $2"
        return 0
    fi
    if [ "${BACKUP:-0}" = 1 ] && [ -f "$2" ]; then
        cp -p "$2" "$2.bak-$(date +%Y%m%d%H%M%S)"
        say "  backup     $2.bak-*"
    fi
    install -m "$3" "$1" "$2.tmp.$$"
    mv -f "$2.tmp.$$" "$2"
    say "  installed  $2 ($3)"
}

# Check results, printed as a table by results_table.
RESULTS_FILE=
results_init() {
    RESULTS_FILE=$(mktemp "${TMPDIR:-/tmp}/cc-bundle.XXXXXX")
    trap 'rm -f "$RESULTS_FILE"' EXIT
    trap 'rm -f "$RESULTS_FILE"; exit 130' INT TERM
}
# record STATUS NAME DETAIL; STATUS is PASS, WARN, FAIL or SKIP.
record() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$RESULTS_FILE"
}
# Prints the table; returns 1 when any check failed.
results_table() {
    echo
    printf '%s\n' "$1"
    awk -F '\t' '{ printf "  %-4s  %-26s %s\n", $1, $2, $3 }' "$RESULTS_FILE"
    fails=$(grep -c '^FAIL' "$RESULTS_FILE" || true)
    warns=$(grep -c '^WARN' "$RESULTS_FILE" || true)
    passes=$(grep -c '^PASS' "$RESULTS_FILE" || true)
    echo
    printf '  %s passed, %s warnings, %s failed\n' "$passes" "$warns" "$fails"
    [ "$fails" -eq 0 ]
}

# sums_check FILE: sha256sum -c FILE, printing only the lines that are not
# OK. Not --quiet, which busybox's sha256sum (Alpine) rejects.
sums_check() {
    if sums_out=$(sha256sum -c "$1" 2>&1); then return 0; fi
    printf '%s\n' "$sums_out" | grep -v ': OK$' || true
    return 1
}

# This machine's identity: its key pair, made here on the
# first install and kept by every reinstall and update. It changes only on
# purpose (make rotate-key, or a confirmed install over a lost key), because
# the viewer holds back a new key's values until it is accepted there.
#
# installed_before: true when a publisher was installed in $PREFIX already
# (its manifest or publish.json is there), so a missing key is a lost
# identity rather than a first install.
installed_before() {
    [ -f "$PREFIX/install-manifest.sha256" ] || [ -f "$PREFIX/publish.json" ]
}

# confirm_identity ALIAS: CONFIRM_ALIAS=ALIAS on the make command line, else
# the alias typed at a prompt (identity.sh, which update.sh sources too).
# shellcheck source=identity.sh
. "$BUNDLE_DIR/scripts/identity.sh"

# accept_note ALIAS LINE: how the viewer takes a new public key LINE. Its
# notice names the key id and the first 8 characters of the key.
accept_note() {
    say "  on the viewer, accept this key when cc-usage asks about '$1': key ${2%%:*}, fingerprint $(printf '%s' "${2#*:}" | cut -c1-8)"
}

# manifest_update REL: recomputes the line of $PREFIX/REL in the install
# manifest (appended when missing); every other line is kept byte for byte.
manifest_update() {
    mu_manifest=$PREFIX/install-manifest.sha256
    [ -f "$mu_manifest" ] || return 0
    mu_line=$(cd "$PREFIX" && sha256sum "$1")
    mu_tmp=$(mktemp "$PREFIX/.install-manifest.XXXXXX")
    awk -v rel="$1" -v line="$mu_line" '
        { p = $0; sub(/^[0-9a-fA-F]+ [ *]/, "", p) }
        p == rel { if (!done) print line; done = 1; next }
        { print }
        END { if (!done) print line }' "$mu_manifest" >"$mu_tmp"
    chmod 0600 "$mu_tmp"
    mv -f "$mu_tmp" "$mu_manifest"
}

# Runs a command with a time limit when timeout(1) exists.
with_timeout() {
    secs=$1
    shift
    if have timeout; then timeout "$secs" "$@"; else "$@"; fi
}
