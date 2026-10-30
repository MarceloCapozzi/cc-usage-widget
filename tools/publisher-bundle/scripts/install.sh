#!/bin/sh
# make install: copies binaries, creds and config into $PREFIX, makes this
# machine's key pair on the first install (and keeps it on every later one),
# registers the hooks and the status line chain, and starts cc-publish
# (systemd user service, or the ~/.profile fallback with AUTOSTART=profile).
# Idempotent: running it again upgrades in place and restarts the publisher.
set -eu
. ./scripts/lib.sh
umask 077

[ "$INSTALLED_COPY" = 0 ] || die "this is the installed copy in $BUNDLE_DIR; run make install from the original bundle folder"
# Also checked by check-prereqs; repeated for sh scripts/install.sh without make.
refuse_unsafe_run
case $AUTOSTART in
    systemd | profile) ;;
    *) die "AUTOSTART must be systemd or profile, not '$AUTOSTART'" ;;
esac
check_path HOME "$HOME"
check_path PREFIX "$PREFIX"
check_path DATA_DIR "$DATA_DIR"
check_path CLAUDE_DIR "$CLAUDE_DIR"
if [ "$AUTOSTART" = systemd ] && ! systemd_user_ok; then
    die "systemd --user is not available: $(systemd_why_not).
       Start cc-publish from ~/.profile instead:  make autostart-profile"
fi
for f in bin/cc-hook bin/cc-publish "secrets/$CREDS_NAME" config/publish.json config/viewer.pub \
    systemd/cc-publish.service.in scripts/cc-publish-autostart.sh; do
    [ -f "$f" ] || die "$f is missing from the bundle"
done
if [ ! -d "$CLAUDE_DIR" ] && [ "$FORCE" != 1 ]; then
    die "$CLAUDE_DIR does not exist; install Claude Code and run it once first (or FORCE=1)"
fi
# The bundle carries the viewer's public key; this machine's key pair, its
# identity, is made here on the first install and kept by every later one.
# Decided before anything is changed:
#   keep     the key is there and loads (a reinstall);
#   first    no key and nothing installed before: a new one is made;
#   new      no key, but a publisher was installed here: the identity is
#            lost, and a new one needs the confirmation (CONFIRM_ALIAS);
#   replace  the key does not load: replaced with the confirmation too.
KEY_FILE=$PREFIX/keys/publisher.key
# The key is judged with the bundle's cc-publish, so that binary must run
# first: one that cannot (a noexec folder, another architecture) would make
# a good key look broken and offer to replace it.
bin/cc-publish --version >/dev/null 2>&1 ||
    die "bin/cc-publish does not run here ($(bin/cc-publish --version 2>&1 | head -n 1)); nothing was installed. Unpack the bundle in a folder that allows running programs (not a noexec /tmp) and check its architecture"
if [ -f "$KEY_FILE" ]; then
    keygen_err=$(mktemp) || die "mktemp failed"
    if publine=$(bin/cc-publish keygen --if-missing --key "$KEY_FILE" 2>"$keygen_err"); then
        rm -f "$keygen_err"
        KEY_ACTION=keep
    else
        why=$(head -n 3 "$keygen_err" 2>/dev/null); rm -f "$keygen_err"
        warn "$KEY_FILE, this machine's identity, does not load: ${why:-no reason given}"
        confirm_identity "$ALIAS" ||
            die "nothing was installed. Restore $KEY_FILE from a backup and run make install again, or replace it on purpose: make install CONFIRM_ALIAS=$ALIAS"
        KEY_ACTION=replace
    fi
elif installed_before; then
    warn "$KEY_FILE is missing, but a publisher was installed in $PREFIX before: the viewer knows '$ALIAS' by that key"
    confirm_identity "$ALIAS" ||
        die "nothing was installed. Restore $KEY_FILE from a backup and run make install again, or create a new identity on purpose: make install CONFIRM_ALIAS=$ALIAS"
    KEY_ACTION=new
else
    KEY_ACTION=first
fi

step "installing the cc-usage publisher for '$ALIAS' into $PREFIX"
# mkdir and chmod rather than install -d -m: same result under umask 077, and
# it also works on file systems that cannot take modes at creation.
mkdir -p "$PREFIX/bin" "$PREFIX/nats" "$PREFIX/keys" "$PREFIX/bundle" "$DATA_DIR"
chmod 0700 "$PREFIX" "$PREFIX/nats" "$PREFIX/keys" "$PREFIX/bundle" "$DATA_DIR"
chmod 0755 "$PREFIX/bin"

# Binaries (0755, renamed into place so running copies are never truncated).
put_file bin/cc-hook "$PREFIX/bin/cc-hook" 0755
put_file bin/cc-publish "$PREFIX/bin/cc-publish" 0755
put_file scripts/cc-publish-autostart.sh "$LAUNCHER" 0755

# Secrets (0600 in 0700 directories). A different existing file is backed up.
BACKUP=1 put_file "secrets/$CREDS_NAME" "$PREFIX/nats/$CREDS_NAME" 0600
# viewer.pub is not secret; 0600 all the same, like everything in $PREFIX.
BACKUP=1 put_file config/viewer.pub "$PREFIX/keys/viewer.pub" 0600
# The private key is made here and never leaves this machine; only its
# public line is printed.
case $KEY_ACTION in
    keep)
        chmod 0600 "$KEY_FILE"
        say "  kept       this machine's identity ${publine%%:*} ($KEY_FILE)"
        ;;
    first | new | replace)
        if [ "$KEY_ACTION" = replace ]; then
            cp -p "$KEY_FILE" "$KEY_FILE.bak-$(date +%Y%m%d%H%M%S)"
            say "  backup     $KEY_FILE.bak-* (the key that does not load)"
            publine=$("$PREFIX/bin/cc-publish" keygen --force --key "$KEY_FILE" \
                --config config/publish.json --confirm "$ALIAS") || die "cc-publish keygen failed"
        else
            publine=$("$PREFIX/bin/cc-publish" keygen --if-missing --key "$KEY_FILE") || die "cc-publish keygen failed"
        fi
        chmod 0600 "$KEY_FILE"
        say "  generated  this machine's identity ${publine%%:*} in $KEY_FILE (the key pair it seals with)"
        say "  public key $publine"
        accept_note "$ALIAS" "$publine"
        ;;
esac

# publish.json with absolute paths: creds and keys under $PREFIX. Other
# members are copied as they are. cc-publish takes the data dir (where
# cc-hook writes the spool) from CC_USAGE_DATA_DIR, which the unit sets and
# the launcher reads from publisher.env.
tmp=$(mktemp "$PREFIX/.publish.json.XXXXXX")
awk -v prefix="$PREFIX" '
    function abs(v) { return (v ~ /^\//) ? v : prefix "/" v }
    {
        if (match($0, /"(creds|publisher_key|viewer_pub)"[ \t]*:[ \t]*"[^"]*"/)) {
            m = substr($0, RSTART, RLENGTH)
            name = m; sub(/^"/, "", name); sub(/".*$/, "", name)
            val = m; sub(/^"[^"]*"[ \t]*:[ \t]*"/, "", val); sub(/"$/, "", val)
            $0 = substr($0, 1, RSTART - 1) "\"" name "\": \"" abs(val) "\"" substr($0, RSTART + RLENGTH)
        }
        print
    }' config/publish.json >"$tmp"
if grep -qE '"(user|data_dir)"' "$tmp"; then
    rm -f "$tmp"
    die "config/publish.json has members cc-publish rejects (user, data_dir); rebuild the bundle"
fi
put_file "$tmp" "$PREFIX/publish.json" 0600
rm -f "$tmp"
# publisher.env: the environment the ~/.profile launcher gives cc-publish
# (the systemd unit carries the same values). CLAUDE_CONFIG_DIR only for a
# Claude Code folder other than ~/.claude.
tmp=$(mktemp "$PREFIX/.publisher.env.XXXXXX")
{
    printf 'DATA_DIR=%s\n' "$DATA_DIR"
    if [ -n "$PUB_CLAUDE_CONFIG_DIR" ]; then printf 'CLAUDE_CONFIG_DIR=%s\n' "$PUB_CLAUDE_CONFIG_DIR"; fi
} >"$tmp"
put_file "$tmp" "$PUBLISHER_ENV" 0600
rm -f "$tmp"

# A copy of the tooling (no binaries, no secrets) so verify, status and
# uninstall keep working after the bundle folder is deleted.
for d in scripts systemd config; do mkdir -p "$PREFIX/bundle/$d" && chmod 0700 "$PREFIX/bundle/$d"; done
for f in Makefile README.md scripts/*.sh systemd/cc-publish.service.in config/publish.json config/bundle.env; do
    install -m 0600 "$f" "$PREFIX/bundle/$f"
done
# Marks the copy, and records the locations it was installed with (the
# Makefile reads them back, so verify and uninstall from the copy act here).
printf 'PREFIX=%s\nDATA_DIR=%s\nCLAUDE_DIR=%s\nUNIT_DIR=%s\nPROFILE=%s\n' \
    "$PREFIX" "$DATA_DIR" "$CLAUDE_DIR" "$UNIT_DIR" "$PROFILE" >"$PREFIX/bundle/config/installed-copy"

# What was installed, checked again by make verify.
(
    cd "$PREFIX"
    sha256sum bin/cc-hook bin/cc-publish bin/cc-publish-autostart "nats/$CREDS_NAME" \
        keys/publisher.key keys/viewer.pub publish.json publisher.env
) >"$PREFIX/install-manifest.sha256"
chmod 0600 "$PREFIX/install-manifest.sha256"

# Hooks and status line chain in settings.json. cc-hook keeps a timestamped
# backup and is idempotent. It registers its own path, so run the installed copy.
step "registering hooks and the status line chain"
hook_settings "$PREFIX/bin/cc-hook" install

# PATH for the publisher: systemd user services get a minimal PATH, and
# cc-publish runs `claude agents --json`, so add the directories of claude
# and node as found now.
svc_path=
for t in claude node; do
    if p=$(command -v "$t" 2>/dev/null) && [ -n "$p" ]; then
        d=$(dirname "$p")
        if (check_path "$t dir" "$d") 2>/dev/null; then
            case ":$svc_path:" in *":$d:"*) ;; *) svc_path=${svc_path:+$svc_path:}$d ;; esac
        fi
    fi
done
[ -n "$svc_path" ] || warn "claude is not on PATH; the publisher will only see what the hooks report"
for d in "$HOME/.local/bin" /usr/local/bin /usr/bin /bin; do
    case ":$svc_path:" in *":$d:"*) ;; *) svc_path=${svc_path:+$svc_path:}$d ;; esac
done

remove_profile_block() {
    if [ -f "$PROFILE" ] && grep -qF "$MARK_BEGIN" "$PROFILE"; then
        t=$(mktemp "$PROFILE.cc-usage.XXXXXX")
        awk -v b="$MARK_BEGIN" -v e="$MARK_END" '$0 == b { skip = 1; next } $0 == e { skip = 0; next } !skip' "$PROFILE" >"$t"
        # Rewrite in place to keep the file's owner, mode and links.
        cat "$t" >"$PROFILE"
        rm -f "$t"
    fi
}

if [ "$AUTOSTART" = systemd ]; then
    step "installing the systemd user service"
    if [ -x "$LAUNCHER" ]; then "$LAUNCHER" stop >/dev/null 2>&1 || true; fi
    remove_profile_block
    mkdir -p "$UNIT_DIR"
    tmp=$(mktemp "$UNIT_DIR/.cc-publish.XXXXXX")
    if [ -n "$PUB_CLAUDE_CONFIG_DIR" ]; then
        claude_env="Environment=\"CLAUDE_CONFIG_DIR=$PUB_CLAUDE_CONFIG_DIR\""
    else
        claude_env="# (Claude Code uses ~/.claude here, so no CLAUDE_CONFIG_DIR.)"
    fi
    RENDER_VARS="PREFIX DATA_DIR SVC_PATH VERSION ALIAS CLAUDE_ENV" R_PREFIX=$PREFIX R_DATA_DIR=$DATA_DIR \
        R_SVC_PATH=$svc_path R_VERSION=$VERSION R_ALIAS=$ALIAS R_CLAUDE_ENV=$claude_env \
        render systemd/cc-publish.service.in >"$tmp"
    chmod 0644 "$tmp"
    if [ -f "$UNIT_FILE" ] && cmp -s "$tmp" "$UNIT_FILE"; then
        rm -f "$tmp"
        say "  unchanged  $UNIT_FILE"
    else
        mv -f "$tmp" "$UNIT_FILE"
        say "  installed  $UNIT_FILE"
    fi
    systemctl --user daemon-reload
    systemctl --user enable "$UNIT_NAME" >/dev/null 2>&1 || systemctl --user enable "$UNIT_NAME"
    # restart, not start: a second install must pick up new binaries and config.
    systemctl --user restart "$UNIT_NAME"
    sleep 2
    if [ "$(systemctl --user is-active "$UNIT_NAME" 2>/dev/null || true)" != active ]; then
        systemctl --user --no-pager status "$UNIT_NAME" 2>&1 | tail -n 15 || true
        die "cc-publish.service did not stay active; see: journalctl --user -u cc-publish -n 50"
    fi
    say "  cc-publish.service is enabled and active"

    me=$(id -un)
    linger=$(loginctl show-user "$me" --property=Linger --value 2>/dev/null || true)
    if [ "$linger" = yes ]; then
        say "  lingering is enabled: the publisher runs without an open session"
    elif [ "$LINGER" = 1 ]; then
        loginctl enable-linger "$me" && say "  enabled lingering for $me"
    else
        say "  note: the publisher stops when your last session closes. To keep it running"
        say "        after logout and start it at boot:  loginctl enable-linger $me"
        say "        (or: make install LINGER=1)"
    fi
else
    step "installing the ~/.profile autostart (no systemd --user)"
    if [ -f "$UNIT_FILE" ]; then
        if systemd_user_ok; then systemctl --user disable --now "$UNIT_NAME" >/dev/null 2>&1 || true; fi
        rm -f "$UNIT_FILE"
        if systemd_user_ok; then systemctl --user daemon-reload || true; fi
        say "  removed    $UNIT_FILE (switching to ~/.profile)"
    fi
    if [ -f "$PROFILE" ] && [ ! -f "$PROFILE.cc-usage.bak" ]; then cp -p "$PROFILE" "$PROFILE.cc-usage.bak"; fi
    remove_profile_block
    {
        echo "$MARK_BEGIN"
        echo "# Starts the cc-usage publisher once (machines without systemd --user)."
        echo "# Added by 'make autostart-profile'; 'make uninstall' removes this block."
        echo "if [ -x '$LAUNCHER' ]; then"
        echo "    PATH='$svc_path' '$LAUNCHER' start >/dev/null 2>&1 || true"
        echo "fi"
        echo "$MARK_END"
    } >>"$PROFILE"
    say "  added the autostart block to $PROFILE"
    for other in "$HOME/.bash_profile" "$HOME/.bash_login"; do
        if [ -f "$other" ] && ! grep -q '\.profile' "$other"; then
            warn "$other exists and does not source ~/.profile, so bash login shells skip the block; add: . \"\$HOME/.profile\""
        fi
    done
    # zsh and fish login shells never read ~/.profile.
    case ${SHELL:-} in
        */zsh) warn "your login shell is zsh, which does not read ~/.profile; add to ~/.zprofile: emulate sh -c '. \"\$HOME/.profile\"'" ;;
        */fish) warn "your login shell is fish, which does not read ~/.profile; start the publisher from fish's config: $LAUNCHER start" ;;
    esac
    PATH=$svc_path:$PATH "$LAUNCHER" restart
fi

step "installed. Validate with:  make verify"
