#!/bin/sh
# make uninstall [PURGE=1]: stops and removes the publisher, takes the hooks
# and the status line chain out of settings.json (restoring the previous
# status line), and removes binaries and config. Secrets (creds and the key
# files in keys/) are kept unless PURGE=1. Safe to run when nothing, or only
# part, is installed.
set -eu
. ./scripts/lib.sh
# Under sudo this would clean up root's files instead of the user's; and the
# bundle folder must be the user's, as for install.
refuse_unsafe_run

step "uninstalling the cc-usage publisher for '$ALIAS' from $PREFIX"

# 1. Stop the publisher, whichever way it was started.
if [ -f "$UNIT_FILE" ]; then
    if systemd_user_ok; then
        systemctl --user disable --now "$UNIT_NAME" >/dev/null 2>&1 || true
    else
        warn "systemd --user not reachable; removing $UNIT_FILE without stopping the service"
    fi
    rm -f "$UNIT_FILE"
    if systemd_user_ok; then
        systemctl --user daemon-reload || true
        systemctl --user reset-failed "$UNIT_NAME" >/dev/null 2>&1 || true
    fi
    say "  removed    $UNIT_FILE"
fi
if [ -x "$LAUNCHER" ]; then "$LAUNCHER" stop || true; fi
if [ -f "$PROFILE" ] && grep -qF "$MARK_BEGIN" "$PROFILE"; then
    t=$(mktemp "$PROFILE.cc-usage.XXXXXX")
    awk -v b="$MARK_BEGIN" -v e="$MARK_END" '$0 == b { skip = 1; next } $0 == e { skip = 0; next } !skip' "$PROFILE" >"$t"
    cat "$t" >"$PROFILE"
    rm -f "$t"
    say "  removed    the autostart block from $PROFILE"
fi

# 2. Hooks and status line chain (needs the chain record in the data dir, so
#    before anything there is removed).
hook=
if [ -x "$PREFIX/bin/cc-hook" ]; then
    hook=$PREFIX/bin/cc-hook
elif [ "$INSTALLED_COPY" = 0 ] && [ -f bin/cc-hook ]; then
    chmod u+x bin/cc-hook 2>/dev/null || true
    hook=./bin/cc-hook
fi
if [ -n "$hook" ]; then
    step "removing hooks and the status line chain"
    if [ "$CLAUDE_DIR" = "$HOME/.claude" ] || [ -f "$CLAUDE_DIR/settings.json" ]; then
        hook_settings "$hook" uninstall || warn "cc-hook uninstall failed; check $CLAUDE_DIR/settings.json"
    fi
else
    warn "no cc-hook binary found; hooks (if any) stay in $CLAUDE_DIR/settings.json"
fi

# 3. Files.
step "removing files"
for f in bin/cc-hook bin/cc-publish bin/cc-publish-autostart publish.json publisher.env \
    install-manifest.sha256 publish-state.json cc-publish.lock cc-publish.pid cc-publish.log cc-publish.log.1; do
    if [ -e "$PREFIX/$f" ]; then
        rm -f "$PREFIX/$f"
        say "  removed    $PREFIX/$f"
    fi
done
# cc-publish writes its state and lock files to the data dir, which PREFIX
# may not be. The publisher is stopped by now, so nothing holds the lock.
for f in publish-state.json cc-publish.lock; do
    if [ -e "$DATA_DIR/$f" ]; then
        rm -f "$DATA_DIR/$f"
        say "  removed    $DATA_DIR/$f"
    fi
done
rmdir "$PREFIX/.cc-publish.lock" 2>/dev/null || true
rmdir "$PREFIX/bin" 2>/dev/null || true
# The tooling copy; this script may be running from it, which is fine on Linux.
if [ -d "$PREFIX/bundle" ]; then
    rm -rf "$PREFIX/bundle"
    say "  removed    $PREFIX/bundle"
fi

if [ "$PURGE" = 1 ]; then
    for f in "$PREFIX/nats/$CREDS_NAME" "$PREFIX/keys/publisher.key" "$PREFIX/keys/viewer.pub"; do
        rm -f "$f" "$f".bak-*
        say "  removed    $f (and backups)"
    done
    # Runtime data written by cc-hook and cc-publish.
    rm -rf "$DATA_DIR/spool" "$DATA_DIR/live"
    rm -f "$DATA_DIR/statusline-chain.json" "$DATA_DIR/cc-hook.log"
    rmdir "$PREFIX/nats" "$PREFIX/keys" 2>/dev/null || true
    rmdir "$PREFIX" 2>/dev/null || true
    [ "$DATA_DIR" = "$PREFIX" ] || rmdir "$DATA_DIR" 2>/dev/null || true
    say "  purged secrets and runtime data"
else
    say "  kept       $PREFIX/nats/$CREDS_NAME and $PREFIX/keys (this machine's identity, which a reinstall keeps; make uninstall PURGE=1 removes them)"
fi

step "uninstalled"
