#!/bin/sh
# make check-prereqs: is this machine ready for `make install`?
# Prints a PASS/WARN/FAIL table; exits 1 when anything FAILs.
set -eu
. ./scripts/lib.sh
results_init

step "checking prerequisites for the cc-usage publisher ($ALIAS, $ARCH)"

if [ "$INSTALLED_COPY" = 1 ]; then
    record FAIL "bundle" "this is the installed copy in $BUNDLE_DIR (no binaries or secrets); run install from the original bundle folder"
    results_table "Prerequisites" || exit 1
fi

# Operating system and architecture.
os=$(uname -s)
if [ "$os" = Linux ]; then
    detail=$(uname -r)
    is_wsl && detail="$detail (WSL)"
    record PASS "Linux" "$detail"
else
    record FAIL "Linux" "this is $os; the bundle's binaries are for Linux"
fi
# install.sh and uninstall.sh repeat these two checks (refuse_unsafe_run).
if ! why=$(sudo_problem); then
    record FAIL "user" "$why"
elif [ "$(id -u)" = 0 ]; then
    record WARN "user" "running as root: this installs for root's ~/.claude; run make as the user who runs Claude Code"
fi
if why=$(bundle_problem); then
    record PASS "bundle owner" "every file belongs to $(id -un), none is world-writable"
else
    record FAIL "bundle owner" "$why"
fi
harch=$(host_arch)
if [ "$harch" = "$ARCH" ]; then
    record PASS "architecture" "$harch matches the bundle"
else
    record FAIL "architecture" "machine is $harch, bundle is $ARCH; generate a bundle with -Target $harch"
fi

# Tools the scripts use.
missing=
for t in install sha256sum sed awk od grep cmp mktemp tr stat; do
    have "$t" || missing="$missing $t"
done
if [ -z "$missing" ]; then
    record PASS "tools" "install sha256sum sed awk od grep cmp mktemp tr stat"
else
    record FAIL "tools" "missing:$missing (install coreutils, sed, gawk or mawk, grep, diffutils)"
fi
for t in systemctl journalctl loginctl timeout; do
    have "$t" || record WARN "tool $t" "not found; some steps are skipped or need the ~/.profile fallback"
done

# Bundle integrity.
if [ -f SHA256SUMS ] && have sha256sum; then
    if out=$(sums_check SHA256SUMS); then
        record PASS "checksums" "every file matches SHA256SUMS"
    else
        record FAIL "checksums" "$(printf '%s' "$out" | head -n 3 | tr '\n' ' ')- copy the bundle again"
    fi
else
    record FAIL "checksums" "SHA256SUMS missing or sha256sum not installed"
fi

# Binaries: right architecture, static, and they start.
for b in cc-hook cc-publish; do
    f=bin/$b
    if [ ! -f "$f" ]; then
        record FAIL "$b binary" "$f missing from the bundle"
        continue
    fi
    if [ "$(elf_magic "$f")" != 7f454c46 ]; then
        record FAIL "$b binary" "$f is not an ELF executable"
        continue
    fi
    barch=$(machine_arch "$(elf_machine "$f")")
    if [ "$barch" != "$ARCH" ]; then
        record FAIL "$b binary" "$f is for $barch, expected $ARCH"
        continue
    fi
    chmod u+x "$f" 2>/dev/null || true
    if v=$(with_timeout 10 "./$f" --version 2>&1); then
        record PASS "$b binary" "$v ($barch)"
    else
        hint=
        if have findmnt && findmnt -no OPTIONS --target "$BUNDLE_DIR" 2>/dev/null | grep -q noexec; then
            hint=" - $BUNDLE_DIR is on a noexec mount; extract the bundle under \$HOME instead"
        fi
        record FAIL "$b binary" "./$f --version failed: $(printf '%s' "$v" | head -n 1)$hint"
    fi
done
for s in "secrets/$CREDS_NAME" config/publish.json config/viewer.pub; do
    [ -f "$s" ] || record FAIL "bundle files" "$s missing"
done

# Paths the unit file and ~/.profile have to carry.
paths_ok=1
for pair in "HOME=$HOME" "PREFIX=$PREFIX" "DATA_DIR=$DATA_DIR" "CLAUDE_DIR=$CLAUDE_DIR"; do
    name=${pair%%=*}
    value=${pair#*=}
    if ! (check_path "$name" "$value") 2>/dev/null; then
        record FAIL "paths" "$name has a quote, backslash, '\$', '%', backquote or newline, or is relative: $value"
        paths_ok=0
    fi
done
[ "$paths_ok" = 1 ] && record PASS "paths" "PREFIX=$PREFIX"
if [ "$DATA_DIR" != "$HOOK_DATA_DIR" ]; then
    record WARN "data dir" "DATA_DIR=$DATA_DIR, but cc-hook writes its spool to $HOOK_DATA_DIR in this environment; unless Claude Code runs with CC_USAGE_DATA_DIR=$DATA_DIR the publisher sees no hook events"
fi
if [ "$PREFIX" != "$DATA_DIR" ]; then
    record WARN "paths" "PREFIX differs from the cc-hook data dir $DATA_DIR; the spool stays in $DATA_DIR"
fi

# Autostart.
if systemd_user_ok; then
    record PASS "systemd --user" "user manager reachable"
else
    record WARN "systemd --user" "$(systemd_why_not). Use: make autostart-profile"
fi

# Claude Code.
if [ -d "$CLAUDE_DIR" ]; then
    record PASS "Claude Code data" "$CLAUDE_DIR"
elif [ "$FORCE" = 1 ]; then
    record WARN "Claude Code data" "$CLAUDE_DIR missing (FORCE=1); run claude once so hooks have a settings file"
else
    record FAIL "Claude Code data" "$CLAUDE_DIR missing: install Claude Code and run it once, or pass CLAUDE_DIR=... (FORCE=1 to continue anyway)"
fi
if [ -n "$PUB_CLAUDE_CONFIG_DIR" ] && [ "${CLAUDE_CONFIG_DIR:-}" != "$PUB_CLAUDE_CONFIG_DIR" ]; then
    record WARN "Claude Code folder" "CLAUDE_DIR=$CLAUDE_DIR, but this shell has ${CLAUDE_CONFIG_DIR:+CLAUDE_CONFIG_DIR=}${CLAUDE_CONFIG_DIR:-no CLAUDE_CONFIG_DIR}; Claude Code uses $CLAUDE_DIR only when it runs with CLAUDE_CONFIG_DIR=$CLAUDE_DIR (the publisher gets it either way)"
fi
if have claude; then
    record PASS "claude on PATH" "$(command -v claude)"
else
    record WARN "claude on PATH" "not found in this shell; cc-publish lists sessions with 'claude agents', so make sure the service can find it"
fi

# Existing installation, for information.
if [ -x "$PREFIX/bin/cc-publish" ]; then
    old=$("$PREFIX/bin/cc-publish" --version 2>/dev/null || echo unknown)
    record PASS "existing install" "$old in $PREFIX (install upgrades it in place)"
fi

results_table "Prerequisites ($ALIAS, bundle $VERSION, $ARCH)"
