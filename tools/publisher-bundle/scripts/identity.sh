# The confirmation for replacing this machine's identity, shared by lib.sh
# (make install, make rotate-key) and update.sh, which runs from an update
# package without lib.sh and sources this file from its bundle/scripts/.
# POSIX sh, no other helpers: it only needs printf and read.

# confirm_identity ALIAS: the confirmation for replacing ALIAS's identity:
# CONFIRM_ALIAS=ALIAS on the command line, else the alias typed at a prompt
# when stdin is a terminal. Surrounding whitespace of the given alias is
# ignored (as Install.ps1 does on Windows). Prints the warning first;
# returns 1, after saying why, when not confirmed.
confirm_identity() {
    ci_alias=$1
    printf "WARNING: This replaces %s's identity. The viewer will hold back its values until you accept the new key there.\n" "$ci_alias" >&2
    if [ -n "${CONFIRM_ALIAS:-}" ]; then
        ci_given=$CONFIRM_ALIAS
    elif [ -t 0 ]; then
        printf 'Type the alias (%s) to confirm: ' "$ci_alias" >&2
        IFS= read -r ci_given || ci_given=
    else
        printf 'not confirmed: pass CONFIRM_ALIAS=%s (or run it in a terminal and type the alias)\n' "$ci_alias" >&2
        return 1
    fi
    # Leading, then trailing whitespace.
    ci_given=${ci_given#"${ci_given%%[![:space:]]*}"}
    ci_given=${ci_given%"${ci_given##*[![:space:]]}"}
    [ "$ci_given" = "$ci_alias" ] && return 0
    printf "not confirmed: '%s' is not this machine's alias '%s'\n" "$ci_given" "$ci_alias" >&2
    return 1
}
