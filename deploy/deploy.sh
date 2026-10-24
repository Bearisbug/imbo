#!/bin/sh
# Install a release of imbo-server on the hosted machine.
#   deploy/deploy.sh <tag>        e.g. deploy/deploy.sh v0.1.0
# Env: IMBO_DEPLOY_HOST (ssh target, default ubuntu@api.imbo.dev),
#      IMBO_SSH_KEY (default ~/.ssh/imbo-prod.pem).
# Keeps earlier releases in /opt/imbo/releases; rolls the symlink back if
# the new one doesn't answer /v1/health within 60 s.
set -eu
TAG=${1:?usage: deploy.sh <tag>}
HOST=${IMBO_DEPLOY_HOST:-ubuntu@api.imbo.dev}
KEY=${IMBO_SSH_KEY:-$HOME/.ssh/imbo-prod.pem}
DIR=$(cd "$(dirname "$0")" && pwd)
SSH="ssh -i $KEY -o ConnectTimeout=15"

scp -q -i "$KEY" "$DIR/imbo-server.service" "$DIR/imbo-backup.service" "$DIR/imbo-backup.timer" "$DIR/backup.sh" "$HOST:/tmp/"
$SSH "$HOST" sudo TAG="$TAG" sh -s <<'REMOTE'
set -eu
install -m 0644 /tmp/imbo-server.service /tmp/imbo-backup.service /tmp/imbo-backup.timer /etc/systemd/system/
install -m 0755 /tmp/backup.sh /opt/imbo/backup.sh
rm -f /tmp/imbo-server.service /tmp/imbo-backup.service /tmp/imbo-backup.timer /tmp/backup.sh
systemctl daemon-reload
systemctl enable --now imbo-backup.timer >/dev/null

base="https://github.com/Bearisbug/imbo/releases/download/$TAG"
pkg="imbo-server-linux-arm64.tar.gz"
rel="/opt/imbo/releases/$TAG"
tmp=$(mktemp -d)
curl -fsSL -o "$tmp/$pkg" "$base/$pkg"
curl -fsSL -o "$tmp/checksums.txt" "$base/checksums.txt"
(cd "$tmp" && grep " $pkg\$" checksums.txt | sha256sum -c --quiet)
mkdir -p "$rel"
tar -xzf "$tmp/$pkg" -C "$rel"
rm -rf "$tmp"

prev=$(readlink /opt/imbo/current || true)
ln -sfn "$rel" /opt/imbo/current
systemctl enable imbo-server >/dev/null 2>&1
systemctl restart imbo-server
for i in $(seq 1 30); do
  if curl -fsS --max-time 3 --resolve api.imbo.dev:443:127.0.0.1 https://api.imbo.dev/v1/health >/dev/null 2>&1 \
     || curl -fsS --max-time 3 -k https://127.0.0.1/v1/health >/dev/null 2>&1; then
    echo "imbo-server $TAG is up (previous: ${prev:-none})"
    exit 0
  fi
  sleep 2
done
echo "imbo-server $TAG did not come up; rolling back to ${prev:-nothing}" >&2
journalctl -u imbo-server -n 30 --no-pager >&2
if [ -n "$prev" ]; then ln -sfn "$prev" /opt/imbo/current; systemctl restart imbo-server; fi
exit 1
REMOTE
