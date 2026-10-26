#!/bin/sh
# Install a release of imbo-server on the hosted machine, through AWS Systems Manager.
#   deploy/deploy.sh <tag>        e.g. deploy/deploy.sh v0.1.0
# The release workflow runs this for every v* tag; by hand it needs AWS credentials that may send
# AWS-RunShellScript to the instance (AWS_PROFILE=kando). The machine runs deploy/install.sh from the tag,
# which backs up, switches and rolls back on its own; this script then checks the public /v1/health.
# Env: IMBO_INSTANCE (default i-09f359c6e35eba48f), AWS_REGION (default ap-east-1).
set -eu
TAG=${1:?usage: deploy.sh <tag>}
INSTANCE=${IMBO_INSTANCE:-i-09f359c6e35eba48f}
export AWS_REGION="${AWS_REGION:-ap-east-1}"

script="https://raw.githubusercontent.com/Bearisbug/imbo/$TAG/deploy/install.sh"
cmd="f=\$(mktemp) && curl -fsSL -o \"\$f\" $script && sh \"\$f\" $TAG; s=\$?; rm -f \"\$f\"; exit \$s"
id=$(aws ssm send-command --instance-ids "$INSTANCE" --document-name AWS-RunShellScript --comment "imbo-server $TAG" \
  --parameters "$(jq -n --arg c "$cmd" '{commands: [$c], executionTimeout: ["900"]}')" \
  --query Command.CommandId --output text)
echo "SSM command $id: installing $TAG on $INSTANCE"

status=Pending
for _ in $(seq 200); do # the command itself times out after 900 s
  sleep 5
  status=$(aws ssm get-command-invocation --command-id "$id" --instance-id "$INSTANCE" --query Status --output text 2>/dev/null) || continue
  case $status in Pending | InProgress | Delayed) ;; *) break ;; esac
done
aws ssm get-command-invocation --command-id "$id" --instance-id "$INSTANCE" \
  --query '[StandardOutputContent, StandardErrorContent]' --output text
[ "$status" = Success ] || { echo "install on the server ended as $status" >&2; exit 1; }

health=$(curl -fsS --max-time 10 https://api.imbo.dev/v1/health)
case $health in
  *'"name":"imbo-server","version":"'"${TAG#v}"'"'*) echo "https://api.imbo.dev reports imbo-server ${TAG#v}" ;;
  *) echo "https://api.imbo.dev does not report imbo-server ${TAG#v}: $health" >&2; exit 1 ;;
esac
