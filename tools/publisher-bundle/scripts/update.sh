#!/bin/sh
# Updates an installed cc-usage publisher to the binaries and the ~/.profile
# launcher of an update package (tools\make-publisher-bundle.ps1 -Update),
# keeping its secrets, config and service; registers the hooks again for the
# new cc-hook's event list (make hooks), then restarts the publisher and runs
# make verify. The package holds no secrets, so it is the same for every
# Linux machine.
#
#   sh update.sh                      install in the default place
#   sh update.sh PREFIX=/opt/cc-usage when make install was given a PREFIX
#   sh update.sh CONFIRM_ALIAS=<alias> confirms a new identity (below)
#
# It also installs the viewer's public key (viewer.pub, in the package) as
# the viewer_pub file publish.json names. This machine's key pair, its
# identity, is kept as it is. Without one (lost), a new one is made with
# `cc-publish keygen --if-missing` only when confirmed: CONFIRM_ALIAS=<alias>,
# or the alias typed at the prompt in a terminal; otherwise it stops before
# changing anything. A new key's public line is printed, and the viewer asks
# to accept it. Running it again changes nothing but the binaries. The
# tooling copy in <prefix>/bundle (verify, status, hooks, uninstall) is
# refreshed from the package's bundle/ folder.
#
# This file sits at the root of the update package, next to bin/, bundle/,
# viewer.pub and SHA256SUMS. It cannot source lib.sh (which needs a bundle's
# config/), so the checks it shares with the bundle scripts (sudo, folder
# owner, sha256sum -c without --quiet) are copies; the identity confirmation
# comes from bundle/scripts/identity.sh, sourced once SHA256SUMS matches.
set -eu
cd "$(dirname "$0")"
PKG_DIR=$(pwd)

say() { printf '%s\n' "$*"; }
die() {
    printf 'update: %s\n' "$*" >&2
    exit 1
}

# The command line only, like the Makefile's variables: an exported
# CONFIRM_ALIAS must not replace an identity, and an exported PREFIX
# (Termux and build environments set one) must not move the install.
CONFIRM_ALIAS=
PREFIX=
for arg in "$@"; do
    case $arg in
        PREFIX=*) PREFIX=${arg#PREFIX=} ;;
        CONFIRM_ALIAS=*) CONFIRM_ALIAS=${arg#CONFIRM_ALIAS=} ;;
        *) die "unknown argument: $arg (usage: sh update.sh [PREFIX=<install dir>] [CONFIRM_ALIAS=<alias>])" ;;
    esac
done
: "${HOME:?HOME is not set}"
# The same default as the Makefile's.
if [ -z "$PREFIX" ]; then
    if [ -n "${CC_USAGE_DATA_DIR:-}" ]; then
        PREFIX=$CC_USAGE_DATA_DIR
    else
        case ${XDG_DATA_HOME:-} in
            /*) PREFIX=$XDG_DATA_HOME/cc-usage ;;
            *) PREFIX=$HOME/.local/share/cc-usage ;;
        esac
    fi
fi
# Relative would mean relative to this package folder, after the cd above.
case $PREFIX in
    /*) ;;
    *) die "PREFIX must be an absolute path: $PREFIX" ;;
esac
# make install refuses quotes, backslashes and newlines in PREFIX, so a
# PREFIX with them names no install.
NL='
'
case $PREFIX in
    *[\"\\]* | *"$NL"*) die "PREFIX contains a quote, a backslash or a newline: $PREFIX" ;;
esac

# Under sudo, HOME and the systemd user manager are root's, and the new
# binaries would belong to root (lib.sh's sudo_problem).
uid=$(id -u)
if [ "$uid" = 0 ] && [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ]; then
    die "running under sudo: this would act for root, not for $SUDO_USER. Run sh update.sh as $SUDO_USER, without sudo"
fi

# The package folder must be the user's and not world-writable, or someone
# else could have swapped its binaries and SHA256SUMS (lib.sh's bundle_problem).
bad=$(find . ! -user "$uid" -print 2>/dev/null | head -n 3 | tr '\n' ' ')
[ -z "$bad" ] || die "files that are not yours (extracted with sudo?): $bad- fix: sudo chown -R \"$(id -un):\" \"$PKG_DIR\""
bad=$(find . ! -type l -perm -0002 -print 2>/dev/null | head -n 3 | tr '\n' ' ')
[ -z "$bad" ] || die "world-writable files or folders (anyone could have replaced them): $bad- extract the package again"

[ -x "$PREFIX/bin/cc-publish" ] && [ -f "$PREFIX/bundle/Makefile" ] ||
    die "no cc-usage publisher installed in $PREFIX (pass PREFIX=<dir> if make install used another one)"

# The install must be this user's too: replacing another user's binaries
# would leave them owned by whoever runs this (root after su, for example).
for p in "$PREFIX" "$PREFIX/bin" "$PREFIX/bin/cc-hook" "$PREFIX/bin/cc-publish" "$PREFIX/bin/cc-publish-autostart" \
    "$PREFIX/install-manifest.sha256" \
    "$PREFIX/publish.json" "$PREFIX/keys" "$PREFIX/keys/publisher.key" "$PREFIX/keys/viewer.pub"; do
    [ -e "$p" ] || continue
    [ -z "$(find "$p" -prune ! -user "$uid" -print 2>/dev/null)" ] ||
        die "$p belongs to another user; run sh update.sh as the user who ran make install"
done

command -v sha256sum >/dev/null 2>&1 || die "sha256sum is missing"
# sums_check DIR FILE: sha256sum -c FILE in DIR, printing only the lines that
# are not OK. Not --quiet, which busybox's sha256sum (Alpine) rejects.
sums_check() {
    if sums_out=$(cd "$1" && sha256sum -c "$2" 2>&1); then return 0; fi
    printf '%s\n' "$sums_out" | grep -v ': OK$' || true
    return 1
}
if ! out=$(sums_check . SHA256SUMS); then
    die "the package does not match its SHA256SUMS; copy it again:
$out"
fi
for f in bin/cc-hook bin/cc-publish viewer.pub bundle/Makefile bundle/scripts/lib.sh bundle/scripts/ctl.sh \
    bundle/scripts/identity.sh bundle/scripts/cc-publish-autostart.sh; do
    [ -f "$f" ] || die "$f is missing from the package"
done
# viewer.pub: <kid>:<base64 of a 32-byte key> lines, comments and blank lines.
if grep -vE '^[[:space:]]*(#.*)?$' viewer.pub | grep -vqE '^[A-Za-z0-9_-]{1,32}:[A-Za-z0-9+/]{43}=$'; then
    die "viewer.pub in the package is not a public key file; rebuild the package"
fi
grep -qE '^[A-Za-z0-9_-]{1,32}:' viewer.pub || die "viewer.pub in the package holds no key; rebuild the package"

others= new_sums= tmp=
cleanup() { rm -f ${others:+"$others"} ${new_sums:+"$new_sums"} ${tmp:+"$tmp"}; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

# This machine's identity, decided before anything is changed. The key files
# go where publish.json says (make install names both, with absolute paths).
cfg=$PREFIX/publish.json
json_get() {
    sed -n 's/^[[:space:]]*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*$/\1/p' "$2" 2>/dev/null | head -n 1
}
alias=$(json_get alias "$cfg")
[ -n "$alias" ] || die "$cfg has no alias; run make install from a new bundle"
key_file=$(json_get publisher_key "$cfg")
pub_file=$(json_get viewer_pub "$cfg")
[ -n "$key_file" ] && [ -n "$pub_file" ] ||
    die "$cfg does not name both key files (publisher_key, viewer_pub); run make install from a new bundle"
for f in "$key_file" "$pub_file"; do
    case $f in
        /*) ;;
        *) die "publish.json names a relative key file ($f); run make install from a new bundle" ;;
    esac
done
# confirm_identity ALIAS, for a machine without a usable key: CONFIRM_ALIAS
# from the command line, else the alias typed at a prompt when stdin is a
# terminal. The bundle scripts' own copy, checked against SHA256SUMS above.
# shellcheck source=identity.sh
. "$PKG_DIR/bundle/scripts/identity.sh"
# key_action: keep (the key loads), new (no key) or replace (it does not
# load); the last two only when confirmed. The package's cc-publish only
# reads an existing key with --if-missing.
# The key is judged with the package's cc-publish, so that binary must run
# first: one that cannot (a noexec folder, another architecture) would make
# a good key look broken and offer to replace it.
bin/cc-publish --version >/dev/null 2>&1 ||
    die "bin/cc-publish of this package does not run here ($(bin/cc-publish --version 2>&1 | head -n 1)); nothing was updated. Unpack it in a folder that allows running programs (not a noexec /tmp) and check its architecture"
keygen_err=$(mktemp) || die "mktemp failed"
if [ -f "$key_file" ] && publine=$(bin/cc-publish keygen --if-missing --key "$key_file" 2>"$keygen_err"); then
    rm -f "$keygen_err"
    key_action=keep
else
    why=$(head -n 3 "$keygen_err" 2>/dev/null); rm -f "$keygen_err"
    if [ -f "$key_file" ]; then
        key_action=replace
        printf "update: %s, this machine's identity, does not load: %s\n" "$key_file" "${why:-no reason given}" >&2
    else
        key_action=new
        printf 'update: %s is missing: the viewer knows this machine by that key\n' "$key_file" >&2
    fi
    confirm_identity "$alias" ||
        die "nothing was updated. Restore $key_file from a backup and run sh update.sh again, or create a new identity on purpose: sh update.sh CONFIRM_ALIAS=$alias"
fi
# The key's path as the manifest lists it: relative to $PREFIX when under it.
case $key_file in
    "$PREFIX"/*) key_rel=${key_file#"$PREFIX"/} ;;
    *) key_rel=$key_file ;;
esac

# make verify compares the installed files with install-manifest.sha256.
# The lines of bin/ (the binaries and the launcher) are recomputed below,
# and so are the lines of the key files this update writes. First make sure
# every other installed file (creds, keys, publish.json, publisher.env)
# still matches it: rewriting the manifest must not hide a change made
# since make install.
manifest=$PREFIX/install-manifest.sha256
is_bin='p == "bin/cc-hook" || p == "bin/cc-publish" || p == "bin/cc-publish-autostart"'
# A manifest line is "<hash>  <path>" (or "<hash> *<path>" in binary mode).
path_of='p = $0; sub(/^[0-9a-fA-F]+ [ *]/, "", p)'
if [ -f "$manifest" ]; then
    others=$(mktemp "$PREFIX/.install-manifest.others.XXXXXX")
    # A key that is missing or does not load is replaced below (confirmed above).
    skip_key=0
    [ "$key_action" = keep ] || skip_key=1
    awk -v key_rel="$key_rel" -v skip_key="$skip_key" \
        "{ $path_of; if ($is_bin) next; if (skip_key && p == key_rel) next; print }" "$manifest" >"$others"
    if [ -s "$others" ] && ! out=$(sums_check "$PREFIX" "${others##*/}"); then
        die "installed files changed since make install (not touched; nothing was updated):
$out
Run make install again from a bundle, which rewrites them and the manifest."
    fi
else
    say "note: $manifest is missing; make verify will report it (make install writes it)"
fi

old=$("$PREFIX/bin/cc-publish" --version 2>/dev/null || echo unknown)
# The binaries and the launcher the ~/.profile block and make restart run
# (install.sh puts scripts/cc-publish-autostart.sh there the same way).
for b in cc-hook cc-publish cc-publish-autostart; do
    src=bin/$b
    [ "$b" = cc-publish-autostart ] && src=bundle/scripts/cc-publish-autostart.sh
    # A copy next to the target, then a rename: the running publisher (and a
    # running launcher) keeps its old file until the restart below.
    install -m 0755 "$src" "$PREFIX/bin/.$b.new"
    mv -f "$PREFIX/bin/.$b.new" "$PREFIX/bin/$b"
done
new=$("$PREFIX/bin/cc-publish" --version 2>/dev/null || echo unknown)
say "updated $PREFIX/bin: $old -> $new"

# The tooling copy in $PREFIX/bundle (verify, status, hooks, uninstall);
# its config/ stays as make install wrote it.
for f in bundle/Makefile bundle/README.md bundle/systemd/cc-publish.service.in bundle/scripts/*.sh; do
    [ -f "$f" ] || continue
    to=$PREFIX/$f
    mkdir -p "$(dirname "$to")"
    install -m 0600 "$f" "$to.new.$$"
    mv -f "$to.new.$$" "$to"
done
say "updated $PREFIX/bundle (scripts only)"

# The viewer's public key, where publish.json says ($pub_file is set above).
mkdir -p "$(dirname "$key_file")" "$(dirname "$pub_file")"
chmod 0700 "$(dirname "$key_file")" "$(dirname "$pub_file")"
if [ -f "$pub_file" ] && cmp -s viewer.pub "$pub_file"; then
    say "unchanged $pub_file"
else
    install -m 0600 viewer.pub "$pub_file.new.$$"
    mv -f "$pub_file.new.$$" "$pub_file"
    say "installed $pub_file (the viewer's public key: values are sealed to it)"
fi
# This machine's identity: kept, or (confirmed above) made here; the
# private key never leaves this machine. Only its public line is printed.
case $key_action in
    keep)
        say "kept this machine's identity ${publine%%:*} ($key_file)"
        ;;
    new | replace)
        if [ "$key_action" = replace ]; then
            cp -p "$key_file" "$key_file.bak-$(date +%Y%m%d%H%M%S)"
            say "backup $key_file.bak-* (the key that does not load)"
            publine=$("$PREFIX/bin/cc-publish" keygen --force --key "$key_file" --config "$cfg" --confirm "$alias") ||
                die "cc-publish keygen failed"
        else
            publine=$("$PREFIX/bin/cc-publish" keygen --if-missing --key "$key_file") || die "cc-publish keygen failed"
        fi
        say "generated this machine's identity ${publine%%:*} in $key_file"
        say "this machine's public key: $publine"
        say "  on the viewer, accept this key when cc-usage asks about '$alias': key ${publine%%:*}, fingerprint $(printf '%s' "${publine#*:}" | cut -c1-8)"
        ;;
esac
chmod 0600 "$key_file"

# Replace the recomputed lines in place (appended if they were missing);
# every other line is kept byte for byte. Paths relative to $PREFIX when
# they are under it.
rel() {
    case $1 in
        "$PREFIX"/*) printf '%s\n' "${1#"$PREFIX"/}" ;;
        *) printf '%s\n' "$1" ;;
    esac
}
if [ -f "$manifest" ]; then
    new_sums=$(mktemp "$PREFIX/.install-manifest.bin.XXXXXX")
    tmp=$(mktemp "$PREFIX/.install-manifest.XXXXXX")
    (
        cd "$PREFIX"
        sha256sum bin/cc-hook bin/cc-publish bin/cc-publish-autostart "$(rel "$key_file")" "$(rel "$pub_file")"
    ) >"$new_sums"
    awk -v new_sums="$new_sums" "
        BEGIN { while ((getline line <new_sums) > 0) { \$0 = line; $path_of; sum[p] = line; order[++n] = p } }
        { $path_of; if (p in sum) { if (!(p in done)) print sum[p]; done[p] = 1; next } print }
        END { for (i = 1; i <= n; i++) if (!(order[i] in done)) print sum[order[i]] }
    " "$manifest" >"$tmp"
    chmod 0600 "$tmp"
    mv -f "$tmp" "$manifest"
fi

# The new cc-hook's event list may differ from the old one's: register the
# hooks again (idempotent; the tooling copy's Makefile names the CLAUDE_DIR
# make install used, so the same settings.json is updated).
make -C "$PREFIX/bundle" hooks
make -C "$PREFIX/bundle" restart
make -C "$PREFIX/bundle" verify
