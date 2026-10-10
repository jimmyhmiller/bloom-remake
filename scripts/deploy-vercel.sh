#!/usr/bin/env bash
# Deploys a Blossom deployment to Vercel as a container function (docs/design/DEPLOY.md §3): `blossom serve` over a
# Postgres database the project has (Neon, from Vercel's marketplace), its page, and a cron job that wakes objects
# every minute.
#
#   scripts/deploy-vercel.sh [--project NAME] [--deploy SPEC] [--schema NAME] [--ref COMMIT] [--database]
#
#   --project   the Vercel project (default blossom-chat; created when missing)
#   --deploy    the deployment spec, relative to the repository (default examples/web/keyed_chat.deploy.toml)
#   --schema    the Postgres schema the deployment's state lives in (default: the spec's deployment id, as an
#               identifier)
#   --ref       the commit to deploy (default HEAD): the image is built from the repository at that commit, so
#               uncommitted changes are not deployed
#   --database  provision a Neon database (free plan, iad1) for the project first
#
# The project's secrets are made once and kept: BLOSSOM_SEED (the deployment's seed: pages' tokens are signed with
# it) and CRON_SECRET (what the cron job shows /blossom/wake). BLOSSOM_STORE is set from the database's direct URL
# (DATABASE_URL_UNPOOLED: the store LISTENs, which a pooled connection cannot) on every deploy, so a rotated password
# is picked up. Needs the Vercel CLI 62 or later (run through npx) and a login (`npx vercel login`).
set -euo pipefail
cd "$(dirname "$0")/.."
repo="$PWD"

project=blossom-chat
deploy=examples/web/keyed_chat.deploy.toml
schema=""
ref=HEAD
database=0
while [ $# -gt 0 ]; do
  case "$1" in
    --project) project="$2"; shift 2 ;;
    --deploy) deploy="$2"; shift 2 ;;
    --schema) schema="$2"; shift 2 ;;
    --ref) ref="$2"; shift 2 ;;
    --database) database=1; shift ;;
    *) echo "deploy-vercel: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -f "$deploy" ] || { echo "deploy-vercel: no deployment spec $deploy" >&2; exit 2; }
if [ -z "$schema" ]; then
  id="$(sed -n 's/^id = "\(.*\)"$/\1/p' "$deploy" | head -1)"
  schema="$(printf '%s' "${id:-blossom}" | tr 'A-Z-' 'a-z_' | tr -cd 'a-z0-9_')"
fi

vercel() { npx -y vercel@62.7.0 "$@"; }
stage="$(mktemp -d "${TMPDIR:-/tmp}/blossom-vercel.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

echo "deploy-vercel: staging $(git rev-parse --short "$ref") of the repository"
git archive "$ref" | tar -x -C "$stage"
cp deploy/vercel/Dockerfile.vercel deploy/vercel/vercel.json "$stage/"
cd "$stage"

vercel project inspect "$project" >/dev/null 2>&1 || vercel project add "$project"
vercel link --yes --project "$project" >/dev/null

if [ "$database" = 1 ]; then
  vercel integration add neon --plan free_v3 -m region=iad1 -m auth=false --name "$project-db" --no-claim
fi

# What the project has (names only; values are pulled below where needed).
have="$(vercel env ls production 2>/dev/null | awk '{print $1}')"
set_env() { # NAME VALUE: replaces the production value
  if printf '%s\n' "$have" | grep -qx "$1"; then vercel env rm "$1" production --yes >/dev/null; fi
  printf '%s' "$2" | vercel env add "$1" production >/dev/null
}
for name in BLOSSOM_SEED CRON_SECRET; do
  if ! printf '%s\n' "$have" | grep -qx "$name"; then
    echo "deploy-vercel: making $name"
    set_env "$name" "$(openssl rand -hex 16)"
  fi
done

vercel env pull --yes --environment production .env.production >/dev/null
direct="$(sed -n 's/^DATABASE_URL_UNPOOLED="\(.*\)"$/\1/p' .env.production)"
rm -f .env.production
if [ -z "$direct" ]; then
  echo "deploy-vercel: the project has no DATABASE_URL_UNPOOLED: give it a Neon database (--database)" >&2
  exit 1
fi
case "$direct" in *\?*) store="$direct&schema=$schema" ;; *) store="$direct?schema=$schema" ;; esac
set_env BLOSSOM_STORE "$store"
set_env BLOSSOM_DEPLOY "$deploy"

echo "deploy-vercel: building and deploying (the image builds on Vercel)"
url="$(vercel deploy --prod --yes | tail -1)"
alias="https://$project.vercel.app"
echo "deploy-vercel: deployed $url"
for _ in $(seq 1 30); do
  if [ "$(curl -s "$alias/blossom/health")" = ok ]; then
    echo "deploy-vercel: healthy at $alias"
    exit 0
  fi
  sleep 2
done
echo "deploy-vercel: $alias/blossom/health did not answer ok" >&2
exit 1
