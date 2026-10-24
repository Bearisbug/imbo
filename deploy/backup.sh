#!/bin/sh
# pg_dump to S3, then touch the stamp file the server reports as backup_age_h.
# Env: IMBO_BACKUP_BUCKET, IMBO_BACKUP_STAMP.
set -eu
name="imbo-$(date -u +%Y%m%dT%H%M%SZ).dump"
tmp=$(mktemp /tmp/imbo-backup.XXXXXX)
trap 'rm -f "$tmp"' EXIT
runuser -u postgres -- pg_dump -Fc imbo > "$tmp"
aws s3 cp --only-show-errors "$tmp" "s3://$IMBO_BACKUP_BUCKET/$name"
touch "$IMBO_BACKUP_STAMP"
chown imbo:imbo "$IMBO_BACKUP_STAMP"
echo "backed up $(stat -c %s "$tmp") bytes to s3://$IMBO_BACKUP_BUCKET/$name"
