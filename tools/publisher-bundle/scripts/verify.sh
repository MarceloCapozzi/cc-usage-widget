#!/bin/sh
# make verify: checks that the installed publisher works end to end and
# prints a PASS/FAIL table. Exits 1 when any check FAILs.
#
# Works from the bundle folder and from the copy in $PREFIX/bundle (after the
# bundle folder, with its secrets, has been deleted).
set -eu
. ./scripts/lib.sh
results_init

step "verifying the cc-usage publisher for '$ALIAS' in $PREFIX"
mode=$(installed_mode)

# 1. Bundle integrity, when this is still the full bundle.
if [ "$INSTALLED_COPY" = 0 ] && [ -f SHA256SUMS ]; then
    if out=$(sums_check SHA256SUMS); then
        record PASS "bundle checksums" "SHA256SUMS"
    else
        record FAIL "bundle checksums" "$(printf '%s' "$out" | head -n 2 | tr '\n' ' ')"
    fi
fi

# 2. Installed files are the ones install wrote.
manifest=$PREFIX/install-manifest.sha256
if [ -f "$manifest" ]; then
    if out=$(cd "$PREFIX" && sums_check install-manifest.sha256); then
        record PASS "installed files" "match install-manifest.sha256"
    else
        record FAIL "installed files" "changed since install: $(printf '%s' "$out" | head -n 2 | tr '\n' ' ')"
    fi
else
    record FAIL "installed files" "$manifest missing; run make install"
fi

# 3. Binaries start.
for b in cc-hook cc-publish; do
    f=$PREFIX/bin/$b
    if [ ! -x "$f" ]; then
        record FAIL "$b --version" "$f missing or not executable"
    elif v=$(with_timeout 10 "$f" --version 2>&1); then
        case $v in
            *"$VERSION"*) record PASS "$b --version" "$v" ;;
            *) record WARN "$b --version" "$v (bundle says $VERSION)" ;;
        esac
    else
        record FAIL "$b --version" "$(printf '%s' "$v" | head -n 1)"
    fi
done

# 4. Permissions of secrets and config.
if [ "$VERIFY_SKIP_MODES" = 1 ]; then
    record SKIP "file modes" "VERIFY_SKIP_MODES=1"
else
    bad=
    for spec in "700:$PREFIX" "700:$PREFIX/nats" "600:$PREFIX/nats/$CREDS_NAME" \
        "700:$PREFIX/keys" "600:$PREFIX/keys/publisher.key" \
        "600:$PREFIX/publish.json" "755:$PREFIX/bin/cc-hook" "755:$PREFIX/bin/cc-publish"; do
        want=${spec%%:*}
        path=${spec#*:}
        if [ ! -e "$path" ]; then
            bad="$bad $path(missing)"
        else
            got=$(file_mode "$path")
            [ "$got" = "$want" ] || bad="$bad $path($got, want $want)"
        fi
    done
    if [ -z "$bad" ]; then
        record PASS "file modes" "secrets 0600 in 0700 dirs, binaries 0755"
    else
        record FAIL "file modes" "$bad - fix with make install"
    fi
fi

# 5. publish.json points at this alias and at files that exist, and the
#    publisher reads the data dir the hooks write to.
cfg=$PREFIX/publish.json
if [ -f "$cfg" ]; then
    calias=$(json_get alias "$cfg")
    ccreds=$(json_get creds "$cfg")
    cdata=$(env_get DATA_DIR "$PUBLISHER_ENV")
    cclaude=$(env_get CLAUDE_CONFIG_DIR "$PUBLISHER_ENV")
    # The key files: as publish.json names them, else cc-publish's
    # defaults under the data dir.
    cpkey=$(json_get publisher_key "$cfg")
    [ -n "$cpkey" ] || cpkey=$cdata/keys/publisher.key
    cvpub=$(json_get viewer_pub "$cfg")
    [ -n "$cvpub" ] || cvpub=$cdata/keys/viewer.pub
    problems=
    [ "$calias" = "$ALIAS" ] || problems="$problems alias=$calias"
    [ -f "$ccreds" ] || problems="$problems creds($ccreds) missing"
    [ -f "$cpkey" ] || problems="$problems publisher_key($cpkey), this machine's identity, missing (restore it from a backup, or make a new one on purpose: make rotate-key)"
    [ -f "$cvpub" ] || problems="$problems viewer_pub($cvpub) missing (copy viewer.pub from the viewer: a new bundle or the update package)"
    if grep -qE '"(user|data_dir)"' "$cfg"; then problems="$problems user/data_dir members (cc-publish rejects them)"; fi
    [ "$cdata" = "$DATA_DIR" ] || problems="$problems publisher.env DATA_DIR=$cdata (cc-hook writes to $DATA_DIR)"
    [ "$cclaude" = "$PUB_CLAUDE_CONFIG_DIR" ] ||
        problems="$problems publisher.env CLAUDE_CONFIG_DIR=${cclaude:-<unset>}, but CLAUDE_DIR is $CLAUDE_DIR (pass the CLAUDE_DIR make install had, or run make verify from $PREFIX/bundle)"
    if [ "$mode" = systemd ] && [ -f "$UNIT_FILE" ]; then
        if [ -n "$PUB_CLAUDE_CONFIG_DIR" ]; then
            grep -qxF "Environment=\"CLAUDE_CONFIG_DIR=$PUB_CLAUDE_CONFIG_DIR\"" "$UNIT_FILE" ||
                problems="$problems the unit does not set CLAUDE_CONFIG_DIR=$PUB_CLAUDE_CONFIG_DIR"
        elif grep -q '^Environment="CLAUDE_CONFIG_DIR=' "$UNIT_FILE"; then
            problems="$problems the unit sets CLAUDE_CONFIG_DIR, but Claude Code's folder is ~/.claude"
        fi
    fi
    if [ -z "$problems" ]; then
        claude_note=
        [ -z "$PUB_CLAUDE_CONFIG_DIR" ] || claude_note=", CLAUDE_CONFIG_DIR $PUB_CLAUDE_CONFIG_DIR"
        record PASS "publish.json" "alias $calias, data dir $cdata$claude_note, both key files present"
    else
        record FAIL "publish.json" "$problems"
    fi
else
    record FAIL "publish.json" "$cfg missing"
fi

# 6. Hooks and status line chain in settings.json.
if [ -x "$PREFIX/bin/cc-hook" ]; then
    st=$(hook_settings "$PREFIX/bin/cc-hook" status 2>&1) || true
    hook_lines=$(printf '%s\n' "$st" | grep -cE '^  [A-Za-z]+ +installed$' || true)
    broken=$(printf '%s\n' "$st" | grep -E '^  [A-Za-z]+ +(missing|duplicated|stray)' | awk '{print $1 "=" $2}' | tr '\n' ' ' || true)
    if printf '%s\n' "$st" | grep -q 'file does not exist'; then
        record FAIL "hooks" "settings.json does not exist; run make hooks"
    elif [ -n "$broken" ]; then
        record FAIL "hooks" "$broken- run make hooks"
    elif [ "$hook_lines" -gt 0 ]; then
        if printf '%s\n' "$st" | grep -qF "$PREFIX/bin/cc-hook"; then
            record PASS "hooks" "$hook_lines events registered to $PREFIX/bin/cc-hook"
        else
            record FAIL "hooks" "registered, but not to $PREFIX/bin/cc-hook; run make hooks"
        fi
    else
        record FAIL "hooks" "cc-hook status shows no installed events"
    fi
    if printf '%s\n' "$st" | grep -qE '^  [^ ].* chained: '; then
        record PASS "status line chain" "$(printf '%s\n' "$st" | sed -n 's/^ *then runs /then runs /p' | head -n 1)"
    elif printf '%s\n' "$st" | grep -q 'not chained'; then
        record WARN "status line chain" "not chained; context use will be estimated (make install chains it)"
    else
        record WARN "status line chain" "cc-hook status did not report the status line"
    fi
else
    record FAIL "hooks" "$PREFIX/bin/cc-hook missing"
fi

# 7. NATS: connect, bind the bucket, probe put/delete with the key. This opens
#    a second connection for a moment (the service holds the first).
if [ -x "$PREFIX/bin/cc-publish" ] && [ -f "$cfg" ]; then
    say "  ...        cc-publish check (NATS connect, bind, probe put/delete)"
    if out=$(pub_exports && with_timeout 60 "$PREFIX/bin/cc-publish" check --config "$cfg" 2>&1); then
        record PASS "cc-publish check" "$(printf '%s\n' "$out" | tail -n 1)"
    else
        record FAIL "cc-publish check" "$(printf '%s\n' "$out" | tail -n 3 | tr '\n' ' ')"
    fi
fi

# 8. Autostart and a recent successful publish.
publish_status() {
    # 0: published recently; 1: not (yet); 2: `status` not supported.
    out=$(pub_exports && with_timeout 20 "$PREFIX/bin/cc-publish" status --config "$cfg" 2>&1) && rc=0 || rc=$?
    case $rc in
        0) return 0 ;;
        2 | 64) return 2 ;;
        *) return 1 ;;
    esac
}
recent_log() {
    if [ "$mode" = systemd ]; then
        journalctl --user -u "$UNIT_NAME" --since "15 min ago" -o cat --no-pager 2>/dev/null || true
    else
        tail -n 200 "$PREFIX/cc-publish.log" 2>/dev/null || true
    fi
}

case $mode in
    systemd)
        if ! systemd_user_ok; then
            record FAIL "service" "$UNIT_FILE exists but systemd --user is not reachable: $(systemd_why_not)"
        else
            en=$(systemctl --user is-enabled "$UNIT_NAME" 2>/dev/null || true)
            ac=$(systemctl --user is-active "$UNIT_NAME" 2>/dev/null || true)
            if [ "$en" = enabled ] && [ "$ac" = active ]; then
                since=$(systemctl --user show -p ActiveEnterTimestamp --value "$UNIT_NAME" 2>/dev/null || true)
                record PASS "service" "cc-publish.service enabled, active since ${since:-?}"
            else
                record FAIL "service" "enabled=$en active=$ac; see make logs"
            fi
            linger=$(loginctl show-user "$(id -un)" --property=Linger --value 2>/dev/null || true)
            if [ "$linger" = yes ]; then
                record PASS "linger" "runs without an open session"
            else
                record WARN "linger" "stops at logout; loginctl enable-linger $(id -un)"
            fi
        fi
        ;;
    profile)
        if [ -x "$LAUNCHER" ] && st=$("$LAUNCHER" status 2>&1); then
            record PASS "autostart" "$PROFILE block; $st"
        else
            record FAIL "autostart" "$PROFILE block present but cc-publish is not running; make restart"
        fi
        ;;
    *)
        record FAIL "autostart" "neither $UNIT_FILE nor the ~/.profile block is installed; run make install"
        ;;
esac

if [ -x "$PREFIX/bin/cc-publish" ] && [ "$mode" != none ]; then
    say "  ...        waiting up to ${VERIFY_WAIT}s for a successful publish"
    waited=0
    result=
    while :; do
        if publish_status; then
            # cc-publish prints "last write  12 s ago (...)"; else its first line.
            lw=$(printf '%s\n' "$out" | sed -n 's/^last write *//p' | head -n 1)
            if [ -n "$lw" ]; then result="pass:last write $lw"; else result="pass:$(printf '%s\n' "$out" | head -n 1)"; fi
        else
            prc=$?
            if [ "$prc" = 2 ]; then
                # No status subcommand: look for a publish in the log instead.
                line=$(recent_log | grep -i 'published' | tail -n 1 || true)
                [ -n "$line" ] && result="pass:log: $line"
            fi
        fi
        [ -n "$result" ] && break
        [ "$waited" -ge "$VERIFY_WAIT" ] && break
        sleep 3
        waited=$((waited + 3))
    done
    if [ -n "$result" ]; then
        record PASS "recent publish" "${result#pass:}"
    else
        last=$(recent_log | tail -n 2 | tr '\n' ' ')
        record FAIL "recent publish" "none within ${VERIFY_WAIT}s. Last log: ${last:-<empty>}"
    fi
fi

results_table "Verification ($ALIAS, $mode)"
