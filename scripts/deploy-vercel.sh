#!/usr/bin/env bash
# Deploys a Blossom deployment to Vercel as a function (docs/design/DEPLOY.md §3): Vercel's Rust runtime runs
# crates/blossom-vercel for every request under /blossom/, the page's files are static, and the state is in a Postgres
# database the project has (Neon, from Vercel's marketplace). Nothing runs between requests.
#
#   scripts/deploy-vercel.sh [--project NAME] [--deploy SPEC] [--schema NAME] [--ref COMMIT] [--database]
#
#   --project   the Vercel project (default blossom-chat; created when missing)
#   --deploy    the deployment spec, relative to the repository (default examples/web/keyed_chat.deploy.toml); its
#               directory goes into the function's bundle
#   --schema    the Postgres schema the deployment's state lives in (default: the spec's deployment id, as an
#               identifier)
#   --ref       the commit whose sources are deployed (default HEAD; uncommitted changes are not deployed). The
#               page's files are built from the working tree (scripts/build-web.sh), which must be at that commit
#   --database  provision a Neon database (free plan, iad1) for the project first
#
# The project's BLOSSOM_SEED (the deployment's seed: pages' tokens are signed with it) is made once and kept.
# BLOSSOM_STORE is set from the database's direct URL (DATABASE_URL_UNPOOLED: the store LISTENs, which a pooled
# connection cannot) on every deploy, so a rotated password is picked up. Needs the Vercel CLI 62 or later (run through
# npx) and a login (`npx vercel login`).
set -euo pipefail
cd "$(dirname "$0")/.."

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
if [ "$(git rev-parse "$ref")" != "$(git rev-parse HEAD)" ] || [ -n "$(git status --porcelain -- web crates/blossom-web)" ]; then
  echo "deploy-vercel: the page is built from the working tree, which is not $ref (or has changes in web/)" >&2
  exit 2
fi
if [ -z "$schema" ]; then
  id="$(sed -n 's/^id = "\(.*\)"$/\1/p' "$deploy" | head -1)"
  schema="$(printf '%s' "${id:-blossom}" | tr 'A-Z-' 'a-z_' | tr -cd 'a-z0-9_')"
fi

vercel() { npx -y vercel@62.7.0 "$@"; }
stage="$(mktemp -d "${TMPDIR:-/tmp}/blossom-vercel.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

echo "deploy-vercel: building the page"
scripts/build-web.sh
echo "deploy-vercel: staging $(git rev-parse --short "$ref") of the repository, and the page"
git archive "$ref" | tar -x -C "$stage"
cp -R web/pkg web/pkg-member "$stage/web/"
sed "s#DEPLOY_DIR#$(dirname "$deploy")#" deploy/vercel/vercel.json > "$stage/vercel.json"
cd "$stage"

vercel project inspect "$project" >/dev/null 2>&1 || vercel project add "$project"
vercel link --yes --project "$project" >/dev/null

if [ "$database" = 1 ]; then
  vercel integration add neon --plan free_v3 -m region=iad1 -m auth=false --name "$project-db" --no-claim
fi

have="$(vercel env ls production 2>/dev/null | awk '{print $1}')"
set_env() { # NAME VALUE: replaces the production value
  if printf '%s\n' "$have" | grep -qx "$1"; then vercel env rm "$1" production --yes >/dev/null; fi
  printf '%s' "$2" | vercel env add "$1" production >/dev/null
}
if ! printf '%s\n' "$have" | grep -qx BLOSSOM_SEED; then
  echo "deploy-vercel: making BLOSSOM_SEED"
  set_env BLOSSOM_SEED "$(openssl rand -hex 16)"
fi
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

echo "deploy-vercel: deploying (Vercel builds the function)"
out="$(vercel deploy --prod --yes 2>&1)" || { printf '%s\n' "$out" >&2; exit 1; }
alias="$(printf '%s\n' "$out" | sed -n 's/.*Aliased[^h]*\(https:[^ ]*\).*/\1/p' | head -1)"
[ -n "$alias" ] || alias="$(printf '%s\n' "$out" | grep -o 'https://[^ ]*vercel.app' | tail -1)"
echo "deploy-vercel: deployed to $alias"
for _ in $(seq 1 30); do
  answer="$(curl -s "$alias/blossom/health" || true)"
  if [ "$answer" = ok ]; then
    echo "deploy-vercel: healthy at $alias"
    exit 0
  fi
  sleep 2
done
echo "deploy-vercel: $alias/blossom/health answered: $answer" >&2
exit 1
