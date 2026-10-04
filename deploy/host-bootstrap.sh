#!/bin/bash
# Turns a fresh Debian 13 droplet into a goopy-life host: everything the
# deploy, admin-apply.sh and the provisioner expect to find already there.
#
# Two ways to run it, both as root:
#
#   1. Pasted into DigitalOcean's "Startup scripts" box when creating the
#      droplet. cloud-init runs it once, on first boot; the output lands in
#      /var/log/cloud-init-output.log and `cloud-init status --long` says
#      whether it finished.
#   2. Again at any time, to converge a host or finish a first run:
#
#        ssh <admin>@<host> sudo bash -s < deploy/host-bootstrap.sh
#
# Every step checks before it changes anything, so a second run on a host that
# is up to date changes nothing. Each step prints `unchanged`, `updated` or
# `created`; both runs append to /var/log/goopy-bootstrap.log.
#
# NO SECRETS IN HERE. Whatever is pasted into the Startup scripts box is served
# by the metadata endpoint (169.254.169.254) to any process on the droplet, for
# its whole life, and cannot be edited afterwards. Passwords and API tokens are
# set by hand after the first boot; the summary at the end lists what is left.
#
# Not in here, because no script on the host can do it: the droplet itself, the
# DigitalOcean cloud firewall and DNS (#167), and what admin-apply.sh and the
# deploy install. See docs/DEPLOYMENT.md, "Production".
#
# ROOT prefixes the paths of the files this writes. It is empty on a host; the
# tests set it to a scratch directory, and source the script with
# GOOPY_BOOTSTRAP_SOURCED=1 so that nothing runs.
set -euo pipefail

# --- Settings -----------------------------------------------------------------

# The admin account: password sudo, once a password is set (see bootstrap_sudo).
ADMIN_USER=${ADMIN_USER:-southp}
# The service and deploy account; deploy/sudoers.goopy names it.
DEPLOY_USER=${DEPLOY_USER:-goopy}
# Shown in every shell prompt, so the two hosts cannot be mistaken for each other.
ENV_LABEL=${ENV_LABEL:-prod}

# Ghost 6.63.0 requires Node ^22.23.1 || ^24.20.0, and nothing checks it before
# an instance boots (docs/GHOST_PROVISIONER.md). Pinned to what dev runs.
NODE_VERSION=22.23.2-1nodesource1
NODESOURCE_KEY_URL=https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key
# Compared after download: a key served by anyone else is refused.
NODESOURCE_KEY_FPR=6F71F525282841EEDAF851B42F59B5F99B1BE0B4


BASE_PACKAGES=(
    ca-certificates curl gnupg git sqlite3
    fail2ban unattended-upgrades
    nginx
    nodejs
)

ROOT=${ROOT:-}
CHANGED=0
NEEDS_REBOOT=0
WARNINGS=()

export DEBIAN_FRONTEND=noninteractive
# The lock timeout: on first boot, cloud-init's own apt runs (DigitalOcean's
# droplet-agent) and apt-daily can hold the dpkg lock while this starts.
APT=(apt-get -q -y -o DPkg::Lock::Timeout=600
    -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold)

# --- Helpers ------------------------------------------------------------------

say() {
    printf '%-10s %s\n' "$1" "$2"
}

step() {
    printf '\n== %s\n' "$1"
}

warn() {
    printf 'WARNING    %s\n' "$1" >&2
    WARNINGS+=("$1")
}

die() {
    printf 'host-bootstrap: %s\n' "$1" >&2
    exit 1
}

# put_file <path> <mode>: installs stdin at <path>, owned by root, unless the file
# there already says the same. Sets CHANGED to 1 when it wrote, 0 when it did
# not, so the caller can reload only what changed. (A return code would do, but
# under `set -e` a forgotten `if` around it would end the run.) Feed it with a
# redirect, never a pipe: a pipe runs it in a subshell, and CHANGED is lost.
put_file() {
    local dest=$ROOT$1 mode=$2 tmp
    tmp=$(mktemp)
    cat >"$tmp"
    if [[ -f $dest ]] && cmp -s "$tmp" "$dest"; then
        rm -f "$tmp"
        CHANGED=0
        say unchanged "$1"
        return
    fi
    mkdir -p "$(dirname "$dest")"
    install -m "$mode" "$tmp" "$dest"
    rm -f "$tmp"
    CHANGED=1
    say updated "$1"
}

# ensure_line <path> <line>: appends <line> unless the file already has it, whole.
ensure_line() {
    local dest=$ROOT$1
    if [[ -f $dest ]] && grep -qxF -- "$2" "$dest"; then
        CHANGED=0
        return
    fi
    printf '%s\n' "$2" >>"$dest"
    CHANGED=1
    say updated "$1: $2"
}

# Prints the keys in an authorized_keys file, without any options in front.
# DigitalOcean puts the droplet's keys in root's file, and cloud-init may prefix
# them with a `command="echo Please login as ..."` that refuses the login; a
# key copied with that prefix would lock the account out the same way.
extract_keys() {
    grep -oE '(ssh-(ed25519|rsa|dss)|ecdsa-sha2-nistp(256|384|521)|sk-(ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com) [A-Za-z0-9+/=]+( .*)?$' "$1" || true
}

# True when a shadow password field holds no password: locked (`!`), disabled
# (`*`) or empty. Every real hash contains a `$`.
no_password_set() {
    [[ $1 != *'$'* ]]
}

# --- 0. Preflight -------------------------------------------------------------

preflight() {
    [[ $(id -u) == 0 ]] || die "run as root"
    # shellcheck source=/dev/null
    . /etc/os-release
    [[ ${VERSION_CODENAME:-} == trixie ]] || die "written for Debian 13 (trixie), found ${PRETTY_NAME:-unknown}"
    exec > >(tee -a /var/log/goopy-bootstrap.log) 2>&1
    printf '\n#### host-bootstrap %s\n' "$(date -u +%FT%TZ)"
}

# --- 1. Accounts --------------------------------------------------------------

# Both accounts read the journal: on dev neither could, and a gl-serv startup
# crash was invisible except by running the binary by hand.
setup_accounts() {
    step "accounts"
    if getent passwd "$ADMIN_USER" >/dev/null; then
        say unchanged "user $ADMIN_USER"
    else
        useradd --create-home --shell /bin/bash "$ADMIN_USER"
        say created "user $ADMIN_USER"
    fi
    usermod -aG sudo,adm,systemd-journal "$ADMIN_USER"

    if getent passwd "$DEPLOY_USER" >/dev/null; then
        say unchanged "user $DEPLOY_USER"
    else
        useradd --system --create-home --home-dir "/home/$DEPLOY_USER" --shell /bin/bash \
            --comment "goopy service account" "$DEPLOY_USER"
        say created "user $DEPLOY_USER"
    fi
    usermod -aG adm,systemd-journal "$DEPLOY_USER"

    install_keys "$ADMIN_USER"
    install_keys "$DEPLOY_USER"
    bootstrap_sudo
    shell_prompt "$ADMIN_USER"
    shell_prompt "$DEPLOY_USER"
}

# Gives <user> the droplet's ssh keys, the ones DigitalOcean put in root's file.
# A file that is already there is left alone: keys added or removed later stand.
install_keys() {
    local user=$1 home dest keys
    home=$(getent passwd "$user" | cut -d: -f6)
    dest=$home/.ssh/authorized_keys
    if [[ -s $dest ]]; then
        say unchanged "$dest"
        return
    fi
    keys=$(extract_keys /root/.ssh/authorized_keys)
    [[ -n $keys ]] || die "no ssh key in /root/.ssh/authorized_keys to give $user; add one to the droplet. Root login is still enabled."
    install -d -m 700 -o "$user" -g "$user" "$home/.ssh"
    printf '%s\n' "$keys" >"$dest"
    chown "$user:$user" "$dest"
    chmod 600 "$dest"
    say created "$dest"
}

# A password cannot come through here (see the header), so the admin account
# starts with none, and passwordless sudo to set one. Removing that is the first
# manual step, and a re-run does not put it back: it is only written while the
# account still has no password.
bootstrap_sudo() {
    local file=/etc/sudoers.d/90-bootstrap-$ADMIN_USER
    if ! no_password_set "$(getent shadow "$ADMIN_USER" | cut -d: -f2)"; then
        say unchanged "$file (absent: $ADMIN_USER has a password)"
        return
    fi
    put_file "$file" 0440 <<EOF
# Written by deploy/host-bootstrap.sh until $ADMIN_USER has a password:
#   sudo passwd $ADMIN_USER && sudo rm $file
$ADMIN_USER ALL=(ALL) NOPASSWD: ALL
EOF
    if ! visudo -c >/dev/null; then
        rm -f "$file"
        die "sudo rejected $file; removed it"
    fi
}

# Prefixes <user>'s prompt with ENV_LABEL in red. In ~/.bashrc, because Debian's
# ~/.bashrc sets PS1 itself and would override anything in /etc/profile.d.
shell_prompt() {
    local home
    home=$(getent passwd "$1" | cut -d: -f6)
    ensure_line "$home/.bashrc" "PS1=\"\\[\\e[1;97;41m\\] $ENV_LABEL \\[\\e[0m\\] \$PS1\"  # host-bootstrap"
}

# --- 2. Packages --------------------------------------------------------------

setup_packages() {
    step "packages"
    "${APT[@]}" update
    "${APT[@]}" install ca-certificates curl gnupg

    # NodeSource, for the pinned Node. The pin outranks Debian's own nodejs and
    # holds the version through upgrades; moving it is a deliberate edit here.
    local keyring=/etc/apt/keyrings/nodesource.gpg tmp fpr
    if [[ -s $keyring ]]; then
        say unchanged "$keyring"
    else
        tmp=$(mktemp)
        curl -fsSL "$NODESOURCE_KEY_URL" | gpg --dearmor >"$tmp"
        fpr=$(gpg --show-keys --with-colons "$tmp" 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }')
        [[ $fpr == "$NODESOURCE_KEY_FPR" ]] || die "NodeSource key fingerprint is $fpr, expected $NODESOURCE_KEY_FPR"
        install -D -m 644 "$tmp" "$keyring"
        rm -f "$tmp"
        say created "$keyring"
    fi
    put_file /etc/apt/sources.list.d/nodesource.list 0644 <<EOF
deb [signed-by=$keyring] https://deb.nodesource.com/node_22.x nodistro main
EOF
    put_file /etc/apt/preferences.d/nodejs 0644 <<EOF
Package: nodejs
Pin: version $NODE_VERSION
Pin-Priority: 1001
EOF

    "${APT[@]}" update
    "${APT[@]}" full-upgrade
    "${APT[@]}" install "${BASE_PACKAGES[@]}"
    if [[ -f /var/run/reboot-required ]]; then
        NEEDS_REBOOT=1
    fi
}

# --- 3. ssh, fail2ban, unattended upgrades ------------------------------------

render_sshd() {
    cat <<EOF
# Written by deploy/host-bootstrap.sh. sshd keeps the first value it reads, and
# this file sorts before cloud-init's 50-cloud-init.conf, so these win.
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers $ADMIN_USER $DEPLOY_USER
LoginGraceTime 30
X11Forwarding no
AllowAgentForwarding no
AllowTcpForwarding no
PermitTunnel no
ClientAliveInterval 120
EOF
}

setup_ssh() {
    step "ssh"
    local file=/etc/ssh/sshd_config.d/10-goopy.conf home
    # Root login goes off here, so the admin account must be able to get in first.
    home=$(getent passwd "$ADMIN_USER" | cut -d: -f6)
    [[ -s $home/.ssh/authorized_keys ]] || die "$ADMIN_USER has no ssh key; leaving root login on"

    put_file "$file" 0644 < <(render_sshd)
    if [[ $CHANGED == 1 ]]; then
        if ! sshd -t; then
            rm -f "$file"
            die "sshd rejected $file; removed it"
        fi
        systemctl reload ssh
    fi
}

setup_auto_updates() {
    step "fail2ban and unattended upgrades"
    # Debian's own jail.d/defaults-debian.conf already enables the sshd jail,
    # reading the journal and banning through nftables.
    systemctl enable --now fail2ban
    # Debian and Debian-Security only, never an automatic reboot: a reboot drops
    # every visitor's instance. The summary says when one is due.
    put_file /etc/apt/apt.conf.d/20auto-upgrades 0644 <<EOF
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF
    systemctl enable --now unattended-upgrades
}

# --- Finish -------------------------------------------------------------------

summary() {
    step "summary"
    local pw
    say node "$(node --version 2>/dev/null || echo missing)"
    sshd -T 2>/dev/null | grep -E '^(permitrootlogin|passwordauthentication|allowusers) ' | sed 's/^/sshd       /' || true

    if [[ ${#WARNINGS[@]} -gt 0 ]]; then
        printf '\nWarnings:\n'
        printf '  - %s\n' "${WARNINGS[@]}"
    fi

    printf '\nLeft to do by hand (docs/DEPLOYMENT.md, "Production"):\n'
    pw=$(getent shadow "$ADMIN_USER" | cut -d: -f2)
    if no_password_set "$pw"; then
        printf '  - sudo passwd %s && sudo rm /etc/sudoers.d/90-bootstrap-%s\n' "$ADMIN_USER" "$ADMIN_USER"
    fi
    if [[ $NEEDS_REBOOT == 1 ]]; then
        printf '  - sudo reboot, then run this script again\n'
    fi
    printf '  - ./deploy/admin-apply.sh %s@<host> %s, then ./deploy/deploy.sh %s@<host> %s\n' \
        "$ADMIN_USER" "$ENV_LABEL" "$DEPLOY_USER" "$ENV_LABEL"
}

main() {
    preflight
    setup_accounts
    setup_packages
    setup_ssh
    setup_auto_updates
    summary
}

if [[ ${GOOPY_BOOTSTRAP_SOURCED:-0} != 1 ]]; then
    main "$@"
fi
