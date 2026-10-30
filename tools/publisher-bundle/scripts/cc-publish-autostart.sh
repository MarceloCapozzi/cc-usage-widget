#!/bin/sh
# cc-publish-autostart: starts cc-publish in the background once, for
# machines without systemd --user (WSL without systemd, containers). The
# ~/.profile block that `make autostart-profile` adds calls `start`.
#
#   cc-publish-autostart start | stop | restart | status
#
# Installed as <prefix>/bin/cc-publish-autostart; finds everything relative to
# that location. Keeps a pid file and appends output to <prefix>/cc-publish.log
# (rotated at 1 MiB).
set -eu
umask 077
prefix=$(cd "$(dirname "$0")/.." && pwd)
bin=$prefix/bin/cc-publish
config=$prefix/publish.json
pidfile=$prefix/cc-publish.pid
log=$prefix/cc-publish.log
lock=$prefix/.cc-publish.lock

running_pid() {
    [ -f "$pidfile" ] || return 1
    pid=$(cat "$pidfile" 2>/dev/null || true)
    case $pid in '' | *[!0-9]*) return 1 ;; esac
    kill -0 "$pid" 2>/dev/null || return 1
    # The pid may belong to another program by now.
    if [ -r "/proc/$pid/cmdline" ]; then
        tr '\0' ' ' <"/proc/$pid/cmdline" | grep -q cc-publish || return 1
    fi
    echo "$pid"
}

start() {
    if pid=$(running_pid); then
        echo "cc-publish already running (pid $pid)"
        return 0
    fi
    if [ ! -x "$bin" ]; then
        echo "cc-publish-autostart: $bin not found" >&2
        return 1
    fi
    # mkdir is atomic: two shells starting at once launch one publisher.
    if ! mkdir "$lock" 2>/dev/null; then
        # A lock older than a minute is left over from a crash.
        if [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
            rmdir "$lock" 2>/dev/null || true
            mkdir "$lock" 2>/dev/null || return 0
        else
            echo "cc-publish is being started by another shell"
            return 0
        fi
    fi
    if [ -f "$log" ] && [ "$(wc -c <"$log")" -gt 1048576 ]; then mv -f "$log" "$log.1"; fi
    # The hooks' data dir, written by make install (cc-publish's config has none).
    data_dir=$(sed -n 's/^DATA_DIR=//p' "$prefix/publisher.env" 2>/dev/null | head -n 1)
    # Claude Code's folder, only when make install was given one other than ~/.claude.
    claude_dir=$(sed -n 's/^CLAUDE_CONFIG_DIR=//p' "$prefix/publisher.env" 2>/dev/null | head -n 1)
    if [ -n "$claude_dir" ]; then
        CLAUDE_CONFIG_DIR=$claude_dir
        export CLAUDE_CONFIG_DIR
    fi
    if command -v setsid >/dev/null 2>&1; then
        CC_USAGE_DATA_DIR=${data_dir:-$prefix} setsid "$bin" run --config "$config" </dev/null >>"$log" 2>&1 &
    else
        CC_USAGE_DATA_DIR=${data_dir:-$prefix} nohup "$bin" run --config "$config" </dev/null >>"$log" 2>&1 &
    fi
    echo $! >"$pidfile"
    rmdir "$lock" 2>/dev/null || true
    echo "cc-publish started (pid $(cat "$pidfile"), log $log)"
}

stop() {
    if pid=$(running_pid); then
        kill "$pid" 2>/dev/null || true
        i=0
        while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 10 ]; do
            sleep 1
            i=$((i + 1))
        done
        if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" 2>/dev/null || true; fi
        echo "cc-publish stopped (pid $pid)"
    else
        echo "cc-publish is not running"
    fi
    rm -f "$pidfile"
}

case ${1:-} in
    start) start ;;
    stop) stop ;;
    restart)
        stop
        start
        ;;
    status)
        if pid=$(running_pid); then
            echo "cc-publish running (pid $pid)"
        else
            echo "cc-publish not running"
            exit 1
        fi
        ;;
    *)
        echo "usage: cc-publish-autostart start | stop | restart | status" >&2
        exit 2
        ;;
esac
