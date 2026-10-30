#!/bin/sh
# make status | logs | restart, for whichever autostart is installed, and
# make hooks.
set -eu
. ./scripts/lib.sh

mode=$(installed_mode)
cfg=$PREFIX/publish.json

case ${1:-} in
    status)
        step "cc-usage publisher '$ALIAS' ($mode) in $PREFIX"
        case $mode in
            systemd) systemctl --user --no-pager status "$UNIT_NAME" 2>&1 | head -n 12 || true ;;
            profile) "$LAUNCHER" status || true ;;
            *) say "not installed (no $UNIT_FILE and no ~/.profile block)" ;;
        esac
        if [ -x "$PREFIX/bin/cc-publish" ] && [ -f "$cfg" ]; then
            echo
            step "cc-publish status"
            (pub_exports && "$PREFIX/bin/cc-publish" status --config "$cfg" 2>&1) || true
        fi
        if [ -x "$PREFIX/bin/cc-hook" ]; then
            echo
            step "cc-hook status"
            hook_settings "$PREFIX/bin/cc-hook" status 2>&1 || true
        fi
        ;;
    logs)
        case $mode in
            systemd) exec journalctl --user -u "$UNIT_NAME" -n 50 -f ;;
            profile) exec tail -n 50 -F "$PREFIX/cc-publish.log" ;;
            *) die "not installed; run make install" ;;
        esac
        ;;
    restart)
        case $mode in
            systemd)
                systemctl --user restart "$UNIT_NAME"
                sleep 1
                systemctl --user --no-pager status "$UNIT_NAME" 2>&1 | head -n 5 || true
                ;;
            profile) "$LAUNCHER" restart ;;
            *) die "not installed; run make install" ;;
        esac
        ;;
    hooks)
        # Registers the hooks and the status line chain again, for the event
        # list of the installed cc-hook (update.sh runs it after replacing
        # the binaries). cc-hook install is idempotent: it rewrites
        # settings.json, with a timestamped backup, only when something changes.
        [ -x "$PREFIX/bin/cc-hook" ] || die "$PREFIX/bin/cc-hook is not installed; run make install"
        step "registering hooks and the status line chain"
        hook_settings "$PREFIX/bin/cc-hook" install
        ;;
    *)
        die "usage: ctl.sh status | logs | restart | hooks"
        ;;
esac
