#!/usr/bin/env bash
# Tests for deploy/host-bootstrap.sh.
#
# Run: ./deploy/tests/host-bootstrap.test.sh
#
# The script is sourced with GOOPY_BOOTSTRAP_SOURCED=1, so nothing it would do
# to a host runs, and its helpers and renderers are exercised against a scratch
# ROOT. The parts that only a real host can answer (apt, ZFS, sshd, nft)
# are not covered here; the first boot of a throwaway host is their test.
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

# Control flow takes explicit blocks: `if ...; then die ...; fi`, never a
# `[[ ... ]] || die` shorthand that a second statement could later slip out of.
shorthand=$(grep -nE '(\|\||&&)[[:space:]]*(die|warn)\b' "$SCRIPT")
if [[ -z $shorthand ]]; then
    pass bootstrap_uses_explicit_blocks
else
    fail bootstrap_uses_explicit_blocks "$shorthand"
fi

# Sourcing must define the functions and run nothing: no preflight, no output.
out=$(GOOPY_BOOTSTRAP_SOURCED=1 bash -c "source '$SCRIPT'" 2>&1)
if [[ -z $out ]]; then
    pass bootstrap_does_nothing_when_sourced
else
    fail bootstrap_does_nothing_when_sourced "$out"
fi

# The settings come from the committed file only. Taken from the environment, a
# re-run under sudo would fall back to the defaults and lock out an admin
# account the first boot was given another name for.
got=$(ADMIN_USER=alice DNS_PLUGIN=dns-other GOOPY_BOOTSTRAP_SOURCED=1 bash -c "source '$SCRIPT'; echo \$ADMIN_USER \$DNS_PLUGIN")
if [[ $got == "southp dns-digitalocean" ]]; then
    pass settings_ignore_the_environment
else
    fail settings_ignore_the_environment "got: $got"
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

# --- render_with_contrib ------------------------------------------------------

# Debian 13's stock sources, as the trixie image ships them: main only, which
# has no zfs-dkms. Each Components line gains contrib once, and nothing else moves.
cat >"$ROOT/debian.sources" <<'EOF'
Types: deb
URIs: http://deb.debian.org/debian
Suites: trixie trixie-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp

Types: deb
URIs: http://deb.debian.org/debian-security
Suites: trixie-security
Components: main contrib
Signed-By: /usr/share/keyrings/debian-archive-keyring.pgp
EOF
once=$(render_with_contrib "$ROOT/debian.sources")
render_with_contrib "$ROOT/debian.sources" >"$ROOT/debian.sources.1"
twice=$(render_with_contrib "$ROOT/debian.sources.1")
expected=$(sed 's/^Components: main$/Components: main contrib/' "$ROOT/debian.sources")
if [[ $once == "$expected" && $twice == "$once" ]]; then
    pass sources_gain_contrib_once
else
    fail sources_gain_contrib_once "got:" "$once"
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

# --- render_nftables ----------------------------------------------------------

# A `flush ruleset` would take fail2ban's table, and its bans, on every reload.
rules=$(render_nftables 995 994 33)
if grep -qE '^[[:space:]]*flush[[:space:]]+ruleset' <<<"$rules"; then
    fail nftables_leaves_other_tables_alone "the ruleset flushes everything"
else
    pass nftables_leaves_other_tables_alone
fi

problems=""
if ! grep -qF 'policy drop;' <<<"$(grep -A1 'chain input' <<<"$rules")"; then
    problems+=" input-not-drop"
fi
if ! grep -qF 'tcp dport { 22, 80, 443 } accept' <<<"$rules"; then
    problems+=" ports"
fi
input_chain=$(sed -n '/chain input/,/^    }$/p' <<<"$rules")
if grep -qE 'dport[^;]*3000' <<<"$input_chain"; then
    problems+=" 3000-open"
fi
if ! grep -qF 'ip daddr 169.254.169.254 meta skuid { 995, 994, 33 } reject' <<<"$rules"; then
    problems+=" metadata"
fi
if ! grep -qF 'ip daddr 127.0.0.0/8 tcp dport 3000 meta skuid 994 reject with tcp reset' <<<"$(sed -n '/chain output/,/^    }$/p' <<<"$rules")"; then
    problems+=" ghost-reaches-gl-serv"
fi
if [[ -z $problems ]]; then
    pass nftables_opens_only_ssh_and_web_and_keeps_services_off_the_metadata_endpoint
else
    fail nftables_opens_only_ssh_and_web_and_keeps_services_off_the_metadata_endpoint "problems:$problems"
fi

# --- render_sysctl ------------------------------------------------------------

sysctl=$(render_sysctl)
if grep -qxF 'kernel.yama.ptrace_scope = 1' <<<"$sysctl" && ! grep -qE '^vm\.' <<<"$sysctl"; then
    pass sysctl_restricts_ptrace_and_leaves_memory_tuning_alone
else
    fail sysctl_restricts_ptrace_and_leaves_memory_tuning_alone "$sysctl"
fi

# --- render_zswap_grub --------------------------------------------------------

# GRUB sources the snippet after the image's own settings; it has to add to the
# command line, not replace it (a cloud image's carries its console settings).
got=$(
    GRUB_CMDLINE_LINUX_DEFAULT="net.ifnames=0 biosdevname=0"
    # As GRUB reads it; eval, because bash 3.2 cannot source a process substitution.
    eval "$(render_zswap_grub)"
    printf '%s' "$GRUB_CMDLINE_LINUX_DEFAULT"
)
expected="net.ifnames=0 biosdevname=0 zswap.enabled=1 zswap.compressor=lzo zswap.zpool=zsmalloc zswap.max_pool_percent=25"
if [[ $got == "$expected" ]]; then
    pass zswap_is_appended_to_the_images_command_line
else
    fail zswap_is_appended_to_the_images_command_line "got: $got"
fi

# --- zswap_wants --------------------------------------------------------------

# A zswap that is on is not enough: it must be on with what #113 measured, and
# switched on only after the rest is right.
right=$(zswap_wants lzo zsmalloc 25 Y)
wrong=$(zswap_wants zstd zsmalloc 25 Y)
off=$(zswap_wants "" "" "" "")
if [[ -z $right && $wrong == "compressor lzo" \
    && $off == $'compressor lzo\nzpool zsmalloc\nmax_pool_percent 25\nenabled Y' ]]; then
    pass zswap_corrects_each_setting_and_enables_last
else
    fail zswap_corrects_each_setting_and_enables_last "right: '$right'" "wrong: '$wrong'" "off: '$off'"
fi

# --- The pool against prod.toml -----------------------------------------------

# The worst case, every provisioned instance at its quota, must leave the pool
# under 80% of what ZFS can use (it keeps 1/32 back). Raising the caps or the
# quota in prod.toml without growing the pool fails here, not on the host.
toml="$DEPLOY_DIR/config/prod.toml"
max_provisioned=$(sed -nE 's/^max_provisioned[[:space:]]*=[[:space:]]*([0-9]+).*/\1/p' "$toml")
quota_mb=$(sed -nE 's/^quota_mb[[:space:]]*=[[:space:]]*([0-9]+).*/\1/p' "$toml")
worst=$((max_provisioned * quota_mb))
ceiling=$((POOL_SIZE_MIB * 31 * 80 / (32 * 100)))
if [[ -n $max_provisioned && -n $quota_mb && $worst -le $ceiling ]]; then
    pass pool_holds_every_instance_at_its_quota_under_80_percent
else
    fail pool_holds_every_instance_at_its_quota_under_80_percent \
        "max_provisioned=$max_provisioned quota_mb=$quota_mb: worst case $worst MiB, ceiling $ceiling MiB of $POOL_SIZE_MIB"
fi

# --- Ghost against prod.toml --------------------------------------------------

# The provisioner links every instance against source_dir; a bootstrap that
# installs a different release leaves it pointing at nothing.
source_dir=$(sed -nE 's/^source_dir[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "$toml")
version=$(sed -nE 's/^version[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "$toml")
node_bin=$(sed -nE 's/^node_bin[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "$toml")
if [[ $source_dir == "$APP_DIR/ghost-$GHOST_VERSION" && $version == "$GHOST_VERSION" && $node_bin == /usr/bin/node ]]; then
    pass ghost_install_is_the_one_prod_toml_names
else
    fail ghost_install_is_the_one_prod_toml_names \
        "prod.toml: source_dir=$source_dir version=$version node_bin=$node_bin" \
        "bootstrap: $APP_DIR/ghost-$GHOST_VERSION, node from apt at /usr/bin/node"
fi

# The firewall keeps Ghost off gl-serv's loopback port; a port moved in
# prod.toml alone would leave the rule guarding nothing.
bind_port=$(sed -nE 's/^bind_address[[:space:]]*=[[:space:]]*"[^"]*:([0-9]+)".*/\1/p' "$toml")
if [[ -n $bind_port && $bind_port == "$GL_SERV_PORT" ]]; then
    pass gl_serv_port_is_the_one_prod_toml_binds
else
    fail gl_serv_port_is_the_one_prod_toml_binds \
        "prod.toml: bind_address port=$bind_port" \
        "bootstrap: GL_SERV_PORT=$GL_SERV_PORT"
fi

# Every instance unit runs Ghost under gl-core's INIT_BIN (#195). Debian's tini
# package installs /usr/bin/tini; a host without it fails every spawn.
init_bin=$(sed -nE 's/^pub const INIT_BIN: &str = "([^"]*)";/\1/p' \
    "$DEPLOY_DIR/../backend/gl-core/src/goopy_provisioner/ghost_provisioner.rs")
if [[ $init_bin == /usr/bin/tini && " ${BASE_PACKAGES[*]} " == *" tini "* ]]; then
    pass the_init_instances_run_under_is_installed
else
    fail the_init_instances_run_under_is_installed \
        "gl-core INIT_BIN=$init_bin" \
        "bootstrap BASE_PACKAGES: ${BASE_PACKAGES[*]}"
fi

# Every instance runs as service_user (#187); the bootstrap creates GHOST_USER.
# If the two drift apart, the first deploy is refused on the host, by
# --check-config's account check, instead of here.
service_user=$(sed -nE 's/^service_user[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' "$toml")
if [[ -n $service_user && $service_user == "$GHOST_USER" ]]; then
    pass ghost_user_is_the_one_prod_toml_names
else
    fail ghost_user_is_the_one_prod_toml_names \
        "prod.toml: service_user=$service_user" \
        "bootstrap: GHOST_USER=$GHOST_USER"
fi

rm -rf "$ROOT"

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
