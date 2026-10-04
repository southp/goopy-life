#!/usr/bin/env bash
# Tests for deploy/host-bootstrap.sh.
#
# Run: ./deploy/tests/host-bootstrap.test.sh
#
# The script is sourced with GOOPY_BOOTSTRAP_SOURCED=1, so nothing it would do
# to a host runs, and its helpers and renderers are exercised against a scratch
# ROOT. The parts that only a real droplet can answer (apt, ZFS, sshd, nft)
# are not covered here; the first boot of a throwaway droplet is their test.
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$DEPLOY_DIR/host-bootstrap.sh"
FAILURES=0
CASES=0

pass() {
    CASES=$((CASES + 1))
    echo "ok   — $1"
}

fail() {
    CASES=$((CASES + 1))
    echo "FAIL — $1"
    shift
    printf '       %s\n' "$@"
    FAILURES=$((FAILURES + 1))
}

if bash -n "$SCRIPT"; then
    pass bootstrap_parses
else
    fail bootstrap_parses "bash -n rejected it"
fi

# Sourcing must define the functions and run nothing: no preflight, no output.
out=$(GOOPY_BOOTSTRAP_SOURCED=1 bash -c "source '$SCRIPT'" 2>&1)
if [[ -z $out ]]; then
    pass bootstrap_does_nothing_when_sourced
else
    fail bootstrap_does_nothing_when_sourced "$out"
fi

ROOT=$(mktemp -d)
export ROOT
export GOOPY_BOOTSTRAP_SOURCED=1
# shellcheck source=deploy/host-bootstrap.sh
source "$SCRIPT"
set +e

# --- extract_keys -------------------------------------------------------------

# The shape cloud-init writes into root's file when root login is refused, then
# a plain key, an option-prefixed key, a comment and a blank line.
cat >"$ROOT/authorized_keys" <<'EOF'
no-port-forwarding,no-agent-forwarding,no-X11-forwarding,command="echo 'Please login as the user \"debian\" rather than the user \"root\".';echo;sleep 10;exit 142" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKey1 laptop
ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQKey2 desktop

# a comment
from="10.0.0.1" ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYKey3
EOF
expected='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKey1 laptop
ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQKey2 desktop
ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYKey3'
got=$(extract_keys "$ROOT/authorized_keys")
if [[ $got == "$expected" ]]; then
    pass extract_keys_drops_the_options_in_front_of_each_key
else
    fail extract_keys_drops_the_options_in_front_of_each_key "got:" "$got"
fi

printf '# nothing here\n\n' >"$ROOT/empty_keys"
got=$(extract_keys "$ROOT/empty_keys")
if [[ -z $got ]]; then
    pass extract_keys_finds_nothing_in_a_file_without_keys
else
    fail extract_keys_finds_nothing_in_a_file_without_keys "got: $got"
fi

# --- no_password_set ----------------------------------------------------------

bad=""
for field in '!' '*' '!*' ''; do
    if ! no_password_set "$field"; then
        bad+=" '$field'"
    fi
done
for field in "\$y\$j9T\$salt\$hash" "!\$y\$j9T\$salt\$hash"; do
    if no_password_set "$field"; then
        bad+=" '$field'"
    fi
done
if [[ -z $bad ]]; then
    pass no_password_set_tells_a_hash_from_no_password
else
    fail no_password_set_tells_a_hash_from_no_password "misjudged:$bad"
fi

# --- put_file -----------------------------------------------------------------

put_file /etc/example/conf 0640 <<<one >/dev/null
first=$CHANGED
put_file /etc/example/conf 0640 <<<one >/dev/null
second=$CHANGED
put_file /etc/example/conf 0640 <<<two >/dev/null
third=$CHANGED
mode=$(stat -f %Lp "$ROOT/etc/example/conf" 2>/dev/null || stat -c %a "$ROOT/etc/example/conf")
if [[ $first$second$third == 101 && $(cat "$ROOT/etc/example/conf") == two && $mode == 640 ]]; then
    pass put_file_writes_only_what_differs
else
    fail put_file_writes_only_what_differs "CHANGED per write: $first $second $third (expected 1 0 1), mode $mode"
fi

# --- ensure_line --------------------------------------------------------------

mkdir -p "$ROOT/etc"
printf 'UUID=x / ext4 defaults 0 1\n' >"$ROOT/etc/fstab"
ensure_line /etc/fstab "/swapfile none swap sw 0 0" >/dev/null
ensure_line /etc/fstab "/swapfile none swap sw 0 0" >/dev/null
count=$(grep -cxF "/swapfile none swap sw 0 0" "$ROOT/etc/fstab")
if [[ $count == 1 && $CHANGED == 0 ]]; then
    pass ensure_line_appends_once
else
    fail ensure_line_appends_once "line present $count times, last CHANGED=$CHANGED"
fi

# --- render_sshd --------------------------------------------------------------

sshd=$(render_sshd)
missing=""
for line in "PermitRootLogin no" "PasswordAuthentication no" "AllowUsers $ADMIN_USER $DEPLOY_USER"; do
    if ! grep -qxF "$line" <<<"$sshd"; then
        missing+=" '$line'"
    fi
done
if [[ -z $missing ]]; then
    pass sshd_keeps_root_and_passwords_out_and_names_both_accounts
else
    fail sshd_keeps_root_and_passwords_out_and_names_both_accounts "missing:$missing"
fi

rm -rf "$ROOT"

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
