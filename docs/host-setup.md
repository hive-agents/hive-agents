# Host setup (hive-core)

This guide captures a working host setup flow and the S3/SeaweedFS details that
`hive-core` expects.

## Firewall

```bash
sudo ufw allow ssh
sudo ufw allow http
sudo ufw allow https
sudo ufw enable
```

## Base dependencies

```bash
sudo apt update
sudo apt install -y build-essential ca-certificates curl
```

## Docker + Caddy

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc
sudo tee /etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
Components: stable
Signed-By: /etc/apt/keyrings/docker.asc
EOF

sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list
chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg
chmod o+r /etc/apt/sources.list.d/caddy-stable.list

sudo apt update
sudo apt install -y caddy docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
```

## Rust + JuiceFS

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
curl -sSL https://d.juicefs.com/install | sh -

. ~/.bashrc
```

## Reverse proxy for pairing (/hive-pair)

Pairing is served from the host at `http://127.0.0.1:8081`. Use Caddy to expose
`https://<host>/hive-pair` for clients.

```bash
cat <<'EOF' > /etc/caddy/Caddyfile
foo.hive-agents.xyz {
  handle /hive-pair {
    reverse_proxy localhost:8081
  }
}
EOF
caddy fmt --config /etc/caddy/Caddyfile --overwrite
caddy stop
caddy start --config /etc/caddy/Caddyfile
```

## Install hive-core

```bash
git clone https://github.com/hive-agents/hive-agents.git
cd hive-agents/
cargo run -p hive-core-cli install --root ~/hive-core
```

By default, install starts a local mount at `/home/hive/hive`. Use
`--skip-connect` to skip the local mount.

## S3 / SeaweedFS notes

`hive-core` uses a bundled SeaweedFS S3 endpoint on `127.0.0.1:8333`. The install
command:

- writes `~/hive-core/.env` with `S3_ACCESS_KEY`, `S3_SECRET_KEY`,
  `HIVE_BUCKET`, and `HIVE_VOLUME`
- writes `~/hive-core/state/seaweedfs/s3.json` for SeaweedFS credentials
- formats the JuiceFS volume using those credentials

If SeaweedFS is not responding, check the container logs:

```bash
docker logs hive-core-seaweedfs-1
```

If you need to reset the install:

```bash
docker rm -f $(docker ps -aq) && sudo rm -rf ~/hive-core/
```

Then rerun install, optionally allowing device replacement:

```bash
cargo run -p hive-core-cli install --root ~/hive-core --replace-existing
```

## Export a client envelope

```bash
cargo run -p hive-core-cli connect --root ~/hive-core --host foo.hive-agents.xyz --user hivec
```
