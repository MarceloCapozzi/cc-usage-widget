#!/bin/sh
# make rotate-key: replaces this machine's identity on purpose. Its key pair
# is made on the first install and kept by every reinstall and update; this
# is the one way to change it. The viewer holds back the values of
# a new key until it is accepted there, so it asks for the alias first:
# CONFIRM_ALIAS=<alias> on the make command line, or typed at the prompt.
# Then `cc-publish keygen --rotate` adds a new key in front of the old ones
# (`--force`, after a backup, when the file does not load; a plain keygen
# when it is missing), the manifest line is recomputed, the publisher is
# restarted and the new public key line is printed.
# Works from the bundle folder and from the installed copy in $PREFIX/bundle.
set -eu
. ./scripts/lib.sh
umask 077

if ! why=$(sudo_problem); then die "$why"; fi
cfg=$PREFIX/publish.json
pub=$PREFIX/bin/cc-publish
[ -x "$pub" ] && [ -f "$cfg" ] || die "no cc-usage publisher installed in $PREFIX; run make install first"
alias=$(json_get alias "$cfg")
[ -n "$alias" ] || die "$cfg has no alias"
key_file=$(json_get publisher_key "$cfg")
[ -n "$key_file" ] || die "$cfg names no publisher_key; run make install from a new bundle"
case $key_file in
    /*) ;;
    *) die "$cfg names a relative publisher_key ($key_file); run make install from a new bundle" ;;
esac

step "replacing the identity of '$alias' ($key_file)"
if [ -f "$key_file" ]; then
    if old=$("$pub" keygen --if-missing --key "$key_file" 2>/dev/null); then
        mode=--rotate
        say "  current identity ${old%%:*}"
    else
        mode=--force
        say "  $key_file does not load; it is replaced (a copy is kept as $key_file.bak-<time>)"
    fi
else
    mode=
    say "  $key_file is missing"
fi
confirm_identity "$alias" || die "nothing changed"

if [ "$mode" = --force ]; then
    cp -p "$key_file" "$key_file.bak-$(date +%Y%m%d%H%M%S)"
fi
mkdir -p "$(dirname "$key_file")"
# shellcheck disable=SC2086 # $mode is one flag or nothing
publine=$("$pub" keygen $mode --key "$key_file" --config "$cfg" --confirm "$alias") ||
    die "cc-publish keygen failed"
chmod 0600 "$key_file"
case $key_file in
    "$PREFIX"/*) manifest_update "${key_file#"$PREFIX"/}" ;;
    *) manifest_update "$key_file" ;;
esac
say "  generated  the new identity ${publine%%:*} in $key_file"

if [ "$(installed_mode)" = none ]; then
    warn "no autostart installed; start the publisher with make install"
else
    sh ./scripts/ctl.sh restart
fi
step "this machine's new public key: $publine"
accept_note "$alias" "$publine"
say "  the viewer holds back the values of '$alias' until you accept the new key there"
