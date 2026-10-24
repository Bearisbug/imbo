#!/bin/bash
# EC2 user data for the hosted server (Ubuntu 24.04 arm64): PostgreSQL 18,
# AWS CLI, the imbo user and directories, swap. The program itself is
# installed by deploy.sh.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
hostnamectl set-hostname imbo-prod
apt-get update
apt-get install -y curl ca-certificates gnupg unzip jq
install -d /usr/share/postgresql-common/pgdg
curl -fsSL -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc
echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt noble-pgdg main" > /etc/apt/sources.list.d/pgdg.list
apt-get update
apt-get install -y postgresql-18
curl -fsSL https://awscli.amazonaws.com/awscli-exe-linux-aarch64.zip -o /tmp/awscli.zip
unzip -q /tmp/awscli.zip -d /tmp && /tmp/aws/install && rm -rf /tmp/aws /tmp/awscli.zip
useradd --system --home-dir /var/lib/imbo --create-home --shell /usr/sbin/nologin imbo
runuser -u postgres -- psql -c "CREATE ROLE imbo LOGIN"
runuser -u postgres -- createdb -O imbo imbo
install -d -o imbo -g imbo /var/lib/imbo
install -d /opt/imbo/releases
install -d -m 0750 -o root -g imbo /etc/imbo
fallocate -l 1G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab
mkdir -p /etc/systemd/journald.conf.d
printf '[Journal]\nSystemMaxUse=200M\n' > /etc/systemd/journald.conf.d/size.conf
systemctl restart systemd-journald
touch /var/lib/cloud/imbo-provisioned
