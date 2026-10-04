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
#   2. Again at any time, to converge a host or finish a first run. Copied
#      over rather than piped in, so that sudo has a terminal to ask on:
#
#        scp deploy/host-bootstrap.sh <admin>@<host>:/tmp/
#        ssh -t <admin>@<host> sudo bash /tmp/host-bootstrap.sh
#
# Every step checks before it changes anything, so a second run on a host that
# is up to date changes nothing. Each step prints `unchanged`, `updated` or
# `created`; both runs append to /var/log/goopy-bootstrap.log.
#
# NO SECRETS IN HERE. Whatever is pasted into the Startup scripts box is served
# by the metadata endpoint (169.254.169.254) to any process on the host, for
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
# The account Ghost instances run as once #187 lands: no shell, no home, no sudo.
GHOST_USER=${GHOST_USER:-goopy-ghost}
# Shown in every shell prompt, so the two hosts cannot be mistaken for each other.
ENV_LABEL=${ENV_LABEL:-prod}
# Only for the certificate check and the instructions printed at the end.
DOMAIN=${DOMAIN:-goopy.life}

# Ghost 6.63.0 requires Node ^22.23.1 || ^24.20.0, and nothing checks it before
# an instance boots (docs/GHOST_PROVISIONER.md). Pinned to what dev runs.
NODE_VERSION=22.23.2-1nodesource1
NODESOURCE_KEY_URL=https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key
# Compared after download: a key served by anyone else is refused.
NODESOURCE_KEY_FPR=6F71F525282841EEDAF851B42F59B5F99B1BE0B4

APP_DIR=/opt/goopy-life
# prod.toml's [provisioner] version and source_dir name the same release.
GHOST_VERSION=6.63.0

# 4 GB of swap with zswap is what the capacity caps of 20 were measured on
# (#113); a stock 2 GB droplet serves about 10, and nothing errors at 11. Two
# files because that is exactly what was measured, not because two matter.
SWAPFILES=(/swapfile:1G /swapfile2:3G)
# lzo, not zstd: zstd spawned 7% more and answered 2-3x slower from swap (#113).
ZSWAP_COMPRESSOR=lzo
ZSWAP_ZPOOL=zsmalloc
ZSWAP_MAX_POOL_PERCENT=25

# The pool every instance's dataset is created in (prod.toml [allocator]). The
# binding rule is max_provisioned * quota_mb <= pool size; at 20 * 200 MiB the
# worst case is 3.9 GiB, about 56% of 8 GB (a 5 GB pool would be 90% full, where
# ZFS slows down). Allocated in full, so a root disk that fills up cannot stop
# the pool, which a sparse file would: a pool that cannot write stops every
# instance at once.
POOL=zpool_ghost
POOL_IMG=/var/lib/zfs-ghost.img
POOL_SIZE_MIB=8192

BASE_PACKAGES=(
    ca-certificates curl gnupg git sqlite3
    fail2ban unattended-upgrades nftables
    nginx
    nodejs
    zfs-dkms zfsutils-linux linux-headers-amd64
    snapd
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

    # Created ahead of #187 so that the firewall can name it.
    if getent passwd "$GHOST_USER" >/dev/null; then
        say unchanged "user $GHOST_USER"
    else
        useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin \
            --comment "goopy Ghost instances" "$GHOST_USER"
        say created "user $GHOST_USER"
    fi

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

# --- 2. Swap and zswap --------------------------------------------------------

# Appends to whatever the image's own command line is, rather than replacing it.
render_zswap_grub() {
    cat <<EOF
# Written by deploy/host-bootstrap.sh. Capacity depends on it: see #113.
GRUB_CMDLINE_LINUX_DEFAULT="\$GRUB_CMDLINE_LINUX_DEFAULT zswap.enabled=1 zswap.compressor=$ZSWAP_COMPRESSOR zswap.zpool=$ZSWAP_ZPOOL zswap.max_pool_percent=$ZSWAP_MAX_POOL_PERCENT"
EOF
}

# First, so that everything after it, the ZFS module build and Ghost's install
# among them, already has the swap.
setup_swap() {
    step "swap and zswap"
    local spec path size params=/sys/module/zswap/parameters
    for spec in "${SWAPFILES[@]}"; do
        path=${spec%%:*}
        size=${spec#*:}
        if [[ -e $path ]]; then
            say unchanged "$path"
        else
            fallocate -l "$size" "$path"
            chmod 600 "$path"
            mkswap "$path" >/dev/null
            say created "$path ($size)"
        fi
        ensure_line /etc/fstab "$path none swap sw 0 0"
        if ! swapon --show=NAME --noheadings | grep -qxF "$path"; then
            swapon "$path"
        fi
    done

    put_file /etc/default/grub.d/90-goopy-zswap.cfg 0644 < <(render_zswap_grub)
    if [[ $CHANGED == 1 ]]; then
        update-grub
    fi
    # The command line holds from the next boot; this makes it true now.
    if [[ $(cat "$params/enabled" 2>/dev/null) != Y ]]; then
        if { echo "$ZSWAP_COMPRESSOR" >"$params/compressor" \
            && echo "$ZSWAP_ZPOOL" >"$params/zpool" \
            && echo "$ZSWAP_MAX_POOL_PERCENT" >"$params/max_pool_percent" \
            && echo Y >"$params/enabled"; } 2>/dev/null; then
            say updated "zswap enabled"
        else
            warn "zswap could not be enabled before a reboot"
            NEEDS_REBOOT=1
        fi
    fi
}

# --- 3. Packages --------------------------------------------------------------

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

# --- 4. ssh, fail2ban, unattended upgrades ------------------------------------

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

# --- 5. Kernel, resolver, journal, mail ---------------------------------------

render_sysctl() {
    cat <<EOF
# Written by deploy/host-bootstrap.sh.
#
# Every Ghost instance runs as one account, until #187 the same one as gl-serv:
# without this, any of them can attach a debugger to any other, or to gl-serv.
kernel.yama.ptrace_scope = 1
kernel.kptr_restrict = 2
kernel.dmesg_restrict = 1
# Loose, not strict: strict drops replies that arrive on a different interface
# than the route back, which a reserved IP or the VPC interface can cause.
net.ipv4.conf.all.rp_filter = 2
net.ipv4.conf.default.rp_filter = 2
net.ipv4.conf.all.accept_redirects = 0
net.ipv4.conf.default.accept_redirects = 0
net.ipv6.conf.all.accept_redirects = 0
net.ipv6.conf.default.accept_redirects = 0
net.ipv4.conf.all.send_redirects = 0
net.ipv4.conf.default.send_redirects = 0
net.ipv4.conf.all.accept_source_route = 0
net.ipv6.conf.all.accept_source_route = 0
# Nothing under vm.*: the capacity caps (#113) were measured on its defaults.
EOF
}

harden_host() {
    step "kernel, resolver, journal, mail"
    local file=/etc/sysctl.d/90-goopy.conf
    put_file "$file" 0644 < <(render_sysctl)
    if [[ $CHANGED == 1 ]]; then
        sysctl -q -p "$file"
    fi

    # systemd-resolved answers LLMNR on 0.0.0.0:5355 by default, to the internet.
    put_file /etc/systemd/resolved.conf.d/90-goopy.conf 0644 <<EOF
[Resolve]
LLMNR=no
MulticastDNS=no
EOF
    if [[ $CHANGED == 1 ]]; then
        systemctl restart systemd-resolved
    fi

    put_file /etc/systemd/journald.conf.d/90-goopy.conf 0644 <<EOF
[Journal]
Storage=persistent
SystemMaxUse=500M
EOF
    if [[ $CHANGED == 1 ]]; then
        systemctl restart systemd-journald
    fi

    # DigitalOcean's image ships exim listening on :25, and nothing here sends
    # mail. Masked rather than removed, so nothing that depends on a mail
    # transport goes with it.
    if systemctl cat exim4.service >/dev/null 2>&1 && [[ $(systemctl is-enabled exim4 2>/dev/null) != masked ]]; then
        systemctl mask --now exim4
        say updated "exim4 masked"
    else
        say unchanged "exim4 (masked or absent)"
    fi
}

# --- 6. Firewall --------------------------------------------------------------

# render_nftables <deploy uid> <ghost uid> <www-data uid>
render_nftables() {
    cat <<EOF
#!/usr/sbin/nft -f
# Written by deploy/host-bootstrap.sh. The DigitalOcean cloud firewall in front
# of the droplet says the same; this copy is the one in git.
#
# Not \`flush ruleset\`: fail2ban keeps its bans in a table of its own, and a
# reload must not wipe them. Declaring the table first makes the delete safe on
# a host where it does not exist yet.
table inet goopy
delete table inet goopy

table inet goopy {
    chain input {
        type filter hook input priority filter; policy drop;
        ct state established,related accept
        ct state invalid drop
        iif lo accept
        meta l4proto { icmp, ipv6-icmp } accept
        # ssh, and nginx for the api and every instance. gl-serv's :3000 binds
        # loopback only, and stays unreachable from outside even if that changes.
        tcp dport { 22, 80, 443 } accept
    }

    chain forward {
        type filter hook forward priority filter; policy drop;
    }

    chain output {
        type filter hook output priority filter; policy accept;
        # The metadata endpoint serves this droplet's user data, so this script,
        # to whoever asks. gl-serv, Ghost and nginx never need it.
        # uids: $DEPLOY_USER, $GHOST_USER, www-data
        ip daddr 169.254.169.254 meta skuid { $1, $2, $3 } reject
    }
}
EOF
}

setup_firewall() {
    step "firewall"
    local file=/etc/nftables.conf tmp
    tmp=$(mktemp)
    render_nftables "$(id -u "$DEPLOY_USER")" "$(id -u "$GHOST_USER")" "$(id -u www-data)" >"$tmp"
    nft -c -f "$tmp" || die "nft rejected the rendered ruleset; $file left as it was"
    put_file "$file" 0755 <"$tmp"
    rm -f "$tmp"
    systemctl enable nftables
    # Never `restart`: Debian's unit stops with `nft flush ruleset`, which would
    # take fail2ban's table with it.
    if ! systemctl is-active -q nftables; then
        systemctl start nftables
    elif [[ $CHANGED == 1 ]]; then
        systemctl reload nftables
    fi
}

# --- 7. nginx -----------------------------------------------------------------

setup_nginx() {
    step "nginx"
    local changed
    # A request for a name no site claims, a scan of the bare IP for one, gets
    # no answer at all: no Debian welcome page, no site picked as the default.
    put_file /etc/nginx/sites-available/00-default-deny 0644 <<'EOF'
# Written by deploy/host-bootstrap.sh.
server {
    listen 80 default_server;
    listen [::]:80 default_server;
    server_name _;
    return 444;
}

server {
    listen 443 ssl default_server;
    listen [::]:443 ssl default_server;
    server_name _;
    ssl_reject_handshake on;
}
EOF
    changed=$CHANGED
    if [[ ! -L /etc/nginx/sites-enabled/00-default-deny ]]; then
        ln -s /etc/nginx/sites-available/00-default-deny /etc/nginx/sites-enabled/00-default-deny
        changed=1
    fi
    if [[ -e /etc/nginx/sites-enabled/default || -L /etc/nginx/sites-enabled/default ]]; then
        rm -f /etc/nginx/sites-enabled/default
        say removed /etc/nginx/sites-enabled/default
        changed=1
    fi
    if [[ $changed == 1 ]]; then
        nginx -t || die "nginx rejected 00-default-deny"
        systemctl reload nginx
    fi
}

# --- 8. ZFS pool and the service directory ------------------------------------

setup_zfs() {
    step "ZFS pool and $APP_DIR"
    # The deploy writes here directly, so it must not be root's.
    install -d -m 755 -o "$DEPLOY_USER" -g "$DEPLOY_USER" "$APP_DIR" "$APP_DIR/bin"
    say ok "$APP_DIR and $APP_DIR/bin owned by $DEPLOY_USER"

    # DKMS builds the module for each kernel whose headers are installed. When
    # the upgrade above brought a newer kernel, the running one's headers may be
    # gone from the archive (dev needed them by hand), and the module then loads
    # only after a reboot into the new one. Allowed to fail for that reason.
    "${APT[@]}" install "linux-headers-$(uname -r)" || true
    if ! modprobe zfs 2>/dev/null; then
        warn "no zfs module for the running kernel $(uname -r): reboot, then run this again"
        NEEDS_REBOOT=1
        return
    fi

    if zpool list -H -o name "$POOL" >/dev/null 2>&1; then
        say unchanged "pool $POOL"
    elif [[ -e $POOL_IMG ]]; then
        zpool import -d "$POOL_IMG" "$POOL"
        say imported "pool $POOL from $POOL_IMG"
    else
        fallocate -l "${POOL_SIZE_MIB}M" "$POOL_IMG"
        chmod 600 "$POOL_IMG"
        zpool create -O compression=on -O mountpoint="$APP_DIR/data" "$POOL" "$POOL_IMG"
        say created "pool $POOL, ${POOL_SIZE_MIB} MiB at $POOL_IMG"
    fi
}

# --- 9. certbot ---------------------------------------------------------------

# Debian 13 packages certbot but not its DigitalOcean DNS plugin, so both come
# from snap, as on dev. The certificate itself needs a DigitalOcean API token,
# which cannot pass through here; requesting it is a manual step (see summary).
setup_certbot() {
    step "certbot"
    snap wait system seed.loaded
    if snap list certbot >/dev/null 2>&1; then
        say unchanged "snap certbot"
    else
        snap install --classic certbot
        say created "snap certbot"
    fi
    snap set certbot trust-plugin-with-root=ok
    if snap list certbot-dns-digitalocean >/dev/null 2>&1; then
        say unchanged "snap certbot-dns-digitalocean"
    else
        snap install certbot-dns-digitalocean
        say created "snap certbot-dns-digitalocean"
    fi
    if ! snap connections certbot | grep -qF certbot-dns-digitalocean; then
        snap connect certbot:plugin certbot-dns-digitalocean
    fi
    ln -sfn /snap/bin/certbot /usr/bin/certbot

    put_file /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh 0755 <<'EOF'
#!/bin/sh
# Written by deploy/host-bootstrap.sh: a renewed certificate reaches nginx.
systemctl reload nginx
EOF
}

# --- 10. Ghost base install ---------------------------------------------------

# The shared install every instance links against (docs/GHOST_PROVISIONER.md).
# Built under a .partial name and renamed when complete, so the directory the
# config names exists only once it is whole, and a failed run starts over.
install_ghost() {
    step "Ghost $GHOST_VERSION"
    local dir=$APP_DIR/ghost-$GHOST_VERSION partial home tgz=ghost-$GHOST_VERSION.tgz
    if [[ -f $dir/index.js ]]; then
        say unchanged "$dir"
        return
    fi
    partial=$dir.partial
    rm -rf "$partial"
    install -d -o "$GHOST_USER" -g "$GHOST_USER" "$partial"
    home=$(mktemp -d)
    chown "$GHOST_USER:$GHOST_USER" "$home"
    # As the unprivileged account: the install runs the dependencies' own build
    # scripts, third-party code with no business running as root. corepack
    # fetches the pnpm the release names; nothing is installed globally.
    local as_ghost=(runuser -u "$GHOST_USER" -- env HOME="$home" COREPACK_ENABLE_DOWNLOAD_PROMPT=0)
    (
        cd "$partial"
        "${as_ghost[@]}" npm pack --silent "ghost@$GHOST_VERSION" >/dev/null
        "${as_ghost[@]}" tar xzf "$tgz" --strip-components=1
        rm "$tgz"
        "${as_ghost[@]}" corepack pnpm install --prod
    )
    rm -rf "$home"
    # Read-only to every instance: one that could write here would change the
    # code every other instance runs.
    chown -R root:root "$partial"
    chmod -R a+rX "$partial"
    mv "$partial" "$dir"
    say created "$dir"
}

# --- Finish -------------------------------------------------------------------

summary() {
    step "summary"
    local pw
    say swap "$(free -m | awk '/^Swap:/ { print $2 }') MiB (expect ~4095)"
    say zswap "$(cat /sys/module/zswap/parameters/enabled 2>/dev/null || echo unknown) (expect Y)"
    say pool "$(zpool list -H -o name,size,health "$POOL" 2>/dev/null || echo missing)"
    say node "$(node --version 2>/dev/null || echo missing)"
    say ghost "$(node -p "require('$APP_DIR/ghost-$GHOST_VERSION/package.json').engines.node" 2>/dev/null \
        | sed 's/^/requires node /' || echo missing)"
    if nft list table inet goopy >/dev/null 2>&1; then
        say firewall "inet goopy loaded"
    else
        warn "firewall table inet goopy is not loaded"
    fi
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
    if [[ ! -e /etc/letsencrypt/live/$DOMAIN/fullchain.pem ]]; then
        printf '  - the *.%s certificate: a DigitalOcean token in /etc/letsencrypt/digitalocean.ini, then certbot\n' "$DOMAIN"
    fi
    printf '  - ./deploy/admin-apply.sh %s@<host> %s, then ./deploy/deploy.sh %s@<host> %s\n' \
        "$ADMIN_USER" "$ENV_LABEL" "$DEPLOY_USER" "$ENV_LABEL"

    if [[ $NEEDS_REBOOT == 0 && ${#WARNINGS[@]} == 0 ]]; then
        date -u +%FT%TZ >/etc/goopy-bootstrap
        printf '\nComplete; recorded in /etc/goopy-bootstrap.\n'
    else
        printf '\nINCOMPLETE: see the warnings above.\n'
    fi
}

main() {
    preflight
    setup_accounts
    setup_swap
    setup_packages
    setup_ssh
    setup_auto_updates
    harden_host
    setup_firewall
    setup_nginx
    setup_zfs
    setup_certbot
    install_ghost
    summary
}

if [[ ${GOOPY_BOOTSTRAP_SOURCED:-0} != 1 ]]; then
    main "$@"
fi
