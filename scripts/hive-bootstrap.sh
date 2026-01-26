#!/usr/bin/env bash
set -euo pipefail

log() {
  echo "[hive-bootstrap] $*"
}

OS_NAME="$(uname -s)"
case "${OS_NAME}" in
  Linux)
    OS="linux"
    ;;
  Darwin)
    OS="darwin"
    ;;
  *)
    echo "unsupported OS: ${OS_NAME}" >&2
    exit 1
    ;;
 esac

if [ "$(id -u)" -ne 0 ]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    echo "run as root or install sudo" >&2
    exit 1
  fi
else
  SUDO=""
fi

run() {
  if [ -n "${SUDO}" ]; then
    "${SUDO}" "$@"
  else
    "$@"
  fi
}

IMAGE_MODE="${HIVE_IMAGE_MODE:-runtime}"
PREBUILT_MARKER="${HIVE_PREBUILT_MARKER:-/etc/hive/prebuilt.ok}"
HIVE_PREBUILT_IMAGE="${HIVE_PREBUILT_IMAGE:-0}"
if [ -f "${PREBUILT_MARKER}" ]; then
  HIVE_PREBUILT_IMAGE=1
fi

HIVE_ROOT="${HIVE_ROOT:-/opt/hive-core}"
HIVE_DOMAIN="${HIVE_DOMAIN:-}"
HIVE_VOLUME_ID="${HIVE_VOLUME_ID:-}"
HIVE_VOLUME_MOUNT="${HIVE_VOLUME_MOUNT:-/var/lib/hive}"
HIVE_PREBUILD_INCLUDE_BINARIES="${HIVE_PREBUILD_INCLUDE_BINARIES:-1}"
HIVE_SKIP_CONNECT="${HIVE_SKIP_CONNECT:-1}"
HIVE_SKIP_SSH_USER="${HIVE_SKIP_SSH_USER:-}"
HIVE_SKIP_CADDY="${HIVE_SKIP_CADDY:-}"
HIVE_SKIP_FIREWALL="${HIVE_SKIP_FIREWALL:-}"
HIVE_INSTALL_EXTRA_ARGS="${HIVE_INSTALL_EXTRA_ARGS:-}"

HIVE_CORE_RELEASE_API="${HIVE_CORE_RELEASE_API:-https://api.github.com/repos/hive-agents/hive-agents/releases/latest}"
HIVE_CORE_ASSET="${HIVE_CORE_ASSET:-hive-core-cli-x86_64-unknown-linux-gnu.tar.gz}"
HIVE_PAIRING_ASSET="${HIVE_PAIRING_ASSET:-hive-core-pairing-x86_64-unknown-linux-gnu.tar.gz}"
HIVE_CORE_URL="${HIVE_CORE_URL:-}"
HIVE_PAIRING_URL="${HIVE_PAIRING_URL:-}"
HIVE_BUILD_FROM_SOURCE="${HIVE_BUILD_FROM_SOURCE:-}"

HIVE_AGENTS_GIT_URL="${HIVE_AGENTS_GIT_URL:-https://github.com/hive-agents/hive-agents.git}"
HIVE_AGENTS_GIT_REF="${HIVE_AGENTS_GIT_REF:-master}"
HIVE_CADDY_TEMPLATE_URL="${HIVE_CADDY_TEMPLATE_URL:-}"
HIVE_CADDY_PLACEHOLDER="${HIVE_CADDY_PLACEHOLDER:-__HIVE_DOMAIN__}"
HIVE_CADDYFILE_B64="${HIVE_CADDYFILE_B64:-}"

HIVE_TLS_CERT_B64="${HIVE_TLS_CERT_B64:-}"
HIVE_TLS_KEY_B64="${HIVE_TLS_KEY_B64:-}"
HIVE_TLS_CERT_PATH="${HIVE_TLS_CERT_PATH:-}"
HIVE_TLS_KEY_PATH="${HIVE_TLS_KEY_PATH:-}"

if [ -z "${HIVE_TLS_CERT_PATH}" ] && [ -n "${HIVE_DOMAIN}" ]; then
  HIVE_TLS_CERT_PATH="/etc/certs/${HIVE_DOMAIN}.pem"
fi
if [ -z "${HIVE_TLS_KEY_PATH}" ] && [ -n "${HIVE_DOMAIN}" ]; then
  HIVE_TLS_KEY_PATH="/etc/certs/${HIVE_DOMAIN}-key.pem"
fi

HIVE_CORE_BIN="${HIVE_CORE_BIN:-/usr/local/bin/hive-core}"
HIVE_PAIRING_BIN="${HIVE_PAIRING_BIN:-/usr/local/bin/hive-core-pairing}"

if [ "${OS}" = "darwin" ] && [ -z "${HIVE_BUILD_FROM_SOURCE}" ] && [ -z "${HIVE_CORE_URL}" ]; then
  HIVE_BUILD_FROM_SOURCE=1
fi

if [ "${OS}" = "darwin" ] && [ -z "${HIVE_SKIP_SSH_USER}" ]; then
  HIVE_SKIP_SSH_USER=1
fi

base64_decode() {
  if base64 -d >/dev/null 2>&1 < /dev/null; then
    base64 -d
  else
    base64 -D
  fi
}

install_linux_deps() {
  if [ "${HIVE_PREBUILT_IMAGE}" = "1" ]; then
    return 0
  fi

  log "installing base packages"
  run apt-get update
  run apt-get install -y --no-install-recommends \
    ca-certificates curl gnupg lsb-release \
    ufw sudo git jq \
    build-essential pkg-config libssl-dev \
    openssh-server tmux \
    fuse3 postgresql-client zstd

  log "installing Docker"
  run install -m 0755 -d /etc/apt/keyrings
  run curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  run chmod a+r /etc/apt/keyrings/docker.asc
  run bash -lc 'cat > /etc/apt/sources.list.d/docker.sources <<DOCKER_EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
Components: stable
Signed-By: /etc/apt/keyrings/docker.asc
DOCKER_EOF'

  log "installing Caddy"
  run apt-get install -y debian-keyring debian-archive-keyring apt-transport-https
  run bash -lc "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg"
  run bash -lc "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list"
  run chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  run chmod o+r /etc/apt/sources.list.d/caddy-stable.list

  run apt-get update
  run apt-get install -y caddy docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

  log "installing JuiceFS"
  run bash -lc "curl -sSL https://d.juicefs.com/install | sh -"
}

install_macos_deps() {
  if [ "$(id -u)" -eq 0 ]; then
    echo "run as a non-root user on macOS (sudo is used for privileged steps)" >&2
    exit 1
  fi

  if ! command -v brew >/dev/null 2>&1; then
    log "installing Homebrew"
    NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null || true)"
    eval "$(/usr/local/bin/brew shellenv 2>/dev/null || true)"
  fi

  log "installing brew packages"
  brew install caddy jq juicefs docker docker-compose

  if ! docker info >/dev/null 2>&1; then
    echo "docker is not running; start Docker Desktop or Colima before continuing" >&2
    exit 1
  fi
}

ensure_linux_users() {
  id -u hive >/dev/null 2>&1 || run useradd -m -s /bin/bash hive
  id -u hivec >/dev/null 2>&1 || run useradd -m -s /usr/sbin/nologin hivec

  if getent group docker >/dev/null 2>&1; then
    run usermod -aG docker hive
  fi

  if getent group sudo >/dev/null 2>&1; then
    run usermod -aG sudo hive
  fi
  if ! run test -f /etc/sudoers.d/90-hive; then
    run bash -lc 'echo "hive ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/90-hive'
    run chmod 440 /etc/sudoers.d/90-hive
  fi
}

configure_firewall() {
  if [ -n "${HIVE_SKIP_FIREWALL}" ]; then
    return 0
  fi

  run ufw allow ssh
  run ufw allow http
  run ufw allow https
  run ufw --force enable
}

configure_hivec_sshd() {
  local sshd_config="/etc/ssh/sshd_config"
  if ! run test -f "${sshd_config}"; then
    return 0
  fi

  if run grep -q "^Match User hivec" "${sshd_config}"; then
    return 0
  fi

  run bash -lc "cat >> '${sshd_config}' <<'HIVEC_EOF'

Match User hivec
  AllowTcpForwarding yes
  PermitOpen 127.0.0.1:5432 127.0.0.1:8333
  PermitTTY no
  X11Forwarding no
  AllowAgentForwarding no
HIVEC_EOF"

  if command -v systemctl >/dev/null 2>&1; then
    run systemctl restart ssh || run systemctl restart sshd || true
  else
    run service ssh restart || run service sshd restart || true
  fi
}

resolve_release_asset() {
  local asset_name="$1"
  local url

  url="$(curl -fsSL "${HIVE_CORE_RELEASE_API}" | jq -r --arg name "${asset_name}" '.assets[] | select(.name==$name) | .browser_download_url' | head -n1)"
  if [ -z "${url}" ] || [ "${url}" = "null" ]; then
    echo "missing release asset ${asset_name}" >&2
    exit 1
  fi
  echo "${url}"
}

install_release_binary() {
  local url="$1"
  local binary_name="$2"
  local dest="$3"

  local tarball
  local tmpdir
  tarball="$(mktemp)"
  tmpdir="$(mktemp -d)"

  run curl -fsSL "${url}" -o "${tarball}"
  run tar -xzf "${tarball}" -C "${tmpdir}"

  local bin
  bin="$(find "${tmpdir}" -maxdepth 4 -type f -name "${binary_name}" -print -quit)"
  if [ -z "${bin}" ]; then
    echo "${binary_name} not found in ${url}" >&2
    exit 1
  fi

  run install -m 0755 "${bin}" "${dest}"
  rm -rf "${tarball}" "${tmpdir}"
}

build_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found; install rustup or set HIVE_CORE_URL/HIVE_PAIRING_URL" >&2
    exit 1
  fi

  local repo_dir="${HIVE_SOURCE_DIR:-/tmp/hive-agents-src}"
  if [ ! -d "${repo_dir}/.git" ]; then
    run git clone "${HIVE_AGENTS_GIT_URL}" "${repo_dir}"
  fi

  run bash -lc "cd '${repo_dir}' && git fetch --all"
  if [ -n "${HIVE_AGENTS_GIT_REF}" ]; then
    run bash -lc "cd '${repo_dir}' && git checkout '${HIVE_AGENTS_GIT_REF}'"
  fi

  run bash -lc "cd '${repo_dir}' && cargo build -p hive-core-cli -p hive-core-pairing --release --locked"
  run install -m 0755 "${repo_dir}/target/release/hive-core" "${HIVE_CORE_BIN}"
  run install -m 0755 "${repo_dir}/target/release/hive-core-pairing" "${HIVE_PAIRING_BIN}"
}

ensure_release_binaries() {
  if [ -x "${HIVE_CORE_BIN}" ] && [ -x "${HIVE_PAIRING_BIN}" ]; then
    return 0
  fi

  if [ -z "${HIVE_CORE_URL}" ]; then
    if [ "${OS}" = "linux" ]; then
      HIVE_CORE_URL="$(resolve_release_asset "${HIVE_CORE_ASSET}")"
    fi
  fi
  if [ -z "${HIVE_PAIRING_URL}" ]; then
    if [ "${OS}" = "linux" ]; then
      HIVE_PAIRING_URL="$(resolve_release_asset "${HIVE_PAIRING_ASSET}")"
    fi
  fi

  if [ -z "${HIVE_CORE_URL}" ] || [ -z "${HIVE_PAIRING_URL}" ]; then
    if [ -n "${HIVE_BUILD_FROM_SOURCE}" ]; then
      build_from_source
      return 0
    fi
    echo "missing release URLs; set HIVE_CORE_URL/HIVE_PAIRING_URL or HIVE_BUILD_FROM_SOURCE=1" >&2
    exit 1
  fi

  log "installing hive-core CLI"
  install_release_binary "${HIVE_CORE_URL}" "hive-core" "${HIVE_CORE_BIN}"
  log "installing hive-core pairing"
  install_release_binary "${HIVE_PAIRING_URL}" "hive-core-pairing" "${HIVE_PAIRING_BIN}"
}

maybe_mount_volume() {
  if [ -z "${HIVE_VOLUME_ID}" ]; then
    return 0
  fi

  local vol_dev
  vol_dev="/dev/disk/by-id/scsi-0HC_Volume_${HIVE_VOLUME_ID}"
  run mkdir -p "${HIVE_VOLUME_MOUNT}"

  for _ in $(seq 1 60); do
    if [ -b "${vol_dev}" ]; then
      break
    fi
    sleep 2
  done

  if [ ! -b "${vol_dev}" ]; then
    echo "volume device not found: ${vol_dev}" >&2
    exit 1
  fi

  if ! run blkid "${vol_dev}" >/dev/null 2>&1; then
    run mkfs.ext4 -F "${vol_dev}"
  fi

  local uuid
  uuid="$(run blkid -s UUID -o value "${vol_dev}")"
  if ! run grep -q "${HIVE_VOLUME_MOUNT}" /etc/fstab; then
    run bash -lc "echo 'UUID=${uuid} ${HIVE_VOLUME_MOUNT} ext4 defaults,nofail 0 2' >> /etc/fstab"
  fi
  run mount -a
}

install_tls_certs() {
  if [ -z "${HIVE_TLS_CERT_B64}" ] || [ -z "${HIVE_TLS_KEY_B64}" ]; then
    return 0
  fi

  if [ -z "${HIVE_TLS_CERT_PATH}" ] || [ -z "${HIVE_TLS_KEY_PATH}" ]; then
    echo "TLS paths not set" >&2
    exit 1
  fi

  run install -d -m 750 -o root -g caddy /etc/certs
  printf '%s' "${HIVE_TLS_CERT_B64}" | base64_decode | run tee "${HIVE_TLS_CERT_PATH}" >/dev/null
  printf '%s' "${HIVE_TLS_KEY_B64}" | base64_decode | run tee "${HIVE_TLS_KEY_PATH}" >/dev/null
  run chmod 644 "${HIVE_TLS_CERT_PATH}"
  run chown root:caddy "${HIVE_TLS_KEY_PATH}"
  run chmod 640 "${HIVE_TLS_KEY_PATH}"
}

render_caddyfile() {
  local content

  if [ -n "${HIVE_CADDYFILE_B64}" ]; then
    run install -d -m 755 /etc/caddy
    printf '%s' "${HIVE_CADDYFILE_B64}" | base64_decode > /etc/caddy/Caddyfile
    return 0
  fi

  if [ -z "${HIVE_DOMAIN}" ]; then
    return 0
  fi

  if [ -z "${HIVE_CADDY_TEMPLATE_URL}" ]; then
    HIVE_CADDY_TEMPLATE_URL="https://raw.githubusercontent.com/hive-agents/hive-agents/${HIVE_AGENTS_GIT_REF}/infra/hive-core/Caddyfile.example"
  fi

  content="$(curl -fsSL "${HIVE_CADDY_TEMPLATE_URL}")"
  content="${content//${HIVE_CADDY_PLACEHOLDER}/${HIVE_DOMAIN}}"

  if [ -n "${HIVE_TLS_CERT_PATH}" ] && [ -n "${HIVE_TLS_KEY_PATH}" ] && [ -n "${HIVE_TLS_CERT_B64}" ] && [ -n "${HIVE_TLS_KEY_B64}" ]; then
    content="$(printf '%s\n' "${content}" | awk -v tls_line="  tls ${HIVE_TLS_CERT_PATH} ${HIVE_TLS_KEY_PATH}" 'NR==1 {print; print tls_line; next} {print}')"
  fi

  run install -d -m 755 /etc/caddy
  printf '%s\n' "${content}" | run tee /etc/caddy/Caddyfile >/dev/null
}

restart_caddy() {
  if ! command -v caddy >/dev/null 2>&1; then
    return 0
  fi

  if [ "${OS}" = "linux" ]; then
    run caddy fmt --config /etc/caddy/Caddyfile --overwrite || true
    run systemctl restart caddy || true
  else
    caddy fmt --config /etc/caddy/Caddyfile --overwrite || true
    if command -v brew >/dev/null 2>&1; then
      run brew services restart caddy || true
    fi
  fi
}

run_hive_core_install() {
  if [ ! -x "${HIVE_CORE_BIN}" ]; then
    echo "hive-core binary not found: ${HIVE_CORE_BIN}" >&2
    exit 1
  fi

  local args=(install --root "${HIVE_ROOT}")
  if [ "${HIVE_SKIP_CONNECT}" = "1" ]; then
    args+=(--skip-connect)
  fi
  if [ -n "${HIVE_SKIP_SSH_USER}" ]; then
    args+=(--skip-ssh-user)
  fi
  if [ -x "${HIVE_PAIRING_BIN}" ]; then
    args+=(--pairing-binary "${HIVE_PAIRING_BIN}")
  fi
  if [ -n "${HIVE_INSTALL_EXTRA_ARGS}" ]; then
    if printf '%s' "${HIVE_INSTALL_EXTRA_ARGS}" | grep -q -- "--b2-"; then
      if "${HIVE_CORE_BIN}" install --help | grep -q -- "--b2-endpoint"; then
        read -r -a extra_args <<< "${HIVE_INSTALL_EXTRA_ARGS}"
        args+=("${extra_args[@]}")
      else
        log "hive-core install missing B2 flags; skipping HIVE_INSTALL_EXTRA_ARGS"
      fi
    else
      read -r -a extra_args <<< "${HIVE_INSTALL_EXTRA_ARGS}"
      args+=("${extra_args[@]}")
    fi
  fi

  "${HIVE_CORE_BIN}" "${args[@]}"
}

if [ "${OS}" = "linux" ]; then
  install_linux_deps
  ensure_linux_users
  configure_hivec_sshd
  if [ -z "${HIVE_SKIP_FIREWALL}" ]; then
    configure_firewall
  fi
else
  install_macos_deps
fi

if [ "${IMAGE_MODE}" = "prebuild" ]; then
  log "prebuild mode: writing marker and exiting"
  if [ "${HIVE_PREBUILD_INCLUDE_BINARIES}" = "1" ]; then
    ensure_release_binaries
  fi
  run install -d /etc/hive
  date -Is | run tee "${PREBUILT_MARKER}" >/dev/null
  exit 0
fi

ensure_release_binaries
maybe_mount_volume
install_tls_certs

run_hive_core_install

if [ -z "${HIVE_SKIP_CADDY}" ]; then
  render_caddyfile
  restart_caddy
fi

log "bootstrap complete"
