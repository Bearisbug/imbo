#!/bin/sh
# Install a release of imbo-server on this machine, as root. deploy/deploy.sh runs it here through SSM.
#   install.sh <tag>
# Backs up the database, unpacks the release next to the earlier ones in /opt/imbo/releases, points
# /opt/imbo/current at it and restarts. The release must report itself on /v1/health within 60 s to become
# /opt/imbo/last-good; otherwise current goes back to last-good.
set -eu
TAG=${1:?usage: install.sh <tag>}
raw="https://raw.githubusercontent.com/Bearisbug/imbo/$TAG/deploy"
base="https://github.com/Bearisbug/imbo/releases/download/$TAG"
pkg="imbo-server-linux-arm64.tar.gz"
rel="/opt/imbo/releases/$TAG"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for f in imbo-server.service imbo-backup.service imbo-backup.timer backup.sh; do curl -fsSL -o "$tmp/$f" "$raw/$f"; done
install -m 0644 "$tmp/imbo-server.service" "$tmp/imbo-backup.service" "$tmp/imbo-backup.timer" /etc/systemd/system/
install -m 0755 "$tmp/backup.sh" /opt/imbo/backup.sh
systemctl daemon-reload
systemctl enable --now imbo-backup.timer >/dev/null

curl -fsSL -o "$tmp/$pkg" "$base/$pkg"
curl -fsSL -o "$tmp/checksums.txt" "$base/checksums.txt"
(cd "$tmp" && grep " $pkg\$" checksums.txt | sha256sum -c --quiet)
mkdir -p "$rel"
tar -xzf "$tmp/$pkg" -C "$rel"

# The new release may migrate the database on start.
systemctl start imbo-backup

# healthy <version>: /v1/health answers as imbo-server at that version within 60 s.
healthy() {
  for _ in $(seq 30); do
    h=$(curl -fsS --max-time 3 --resolve api.imbo.dev:443:127.0.0.1 https://api.imbo.dev/v1/health 2>/dev/null) || h=
    case $h in *'"name":"imbo-server","version":"'"$1"'"'*) return 0 ;; esac
    sleep 2
  done
  return 1
}

good=$(readlink /opt/imbo/last-good || readlink /opt/imbo/current || true)
ln -sfn "$rel" /opt/imbo/current
systemctl enable imbo-server >/dev/null 2>&1
systemctl restart imbo-server
if healthy "${TAG#v}"; then
  ln -sfn "$rel" /opt/imbo/last-good
  echo "imbo-server $TAG is up (last good before: ${good:-none})"
  exit 0
fi

echo "imbo-server $TAG did not report itself healthy; rolling back to ${good:-nothing}" >&2
journalctl -u imbo-server -n 30 --no-pager >&2
[ -n "$good" ] || exit 1
ln -sfn "$good" /opt/imbo/current
systemctl restart imbo-server
good_tag=$(basename "$good")
if healthy "${good_tag#v}"; then echo "rolled back to $good_tag" >&2; else echo "the rollback to $good_tag did not come up either" >&2; fi
exit 1
