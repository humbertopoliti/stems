#!/usr/bin/env bash
# scripts/shop-api/create-test-user.sh — shop-api custom "create-test-user"
# script (FR-SC-2). Args: --email (required string), --role (enum
# admin|user, default admin). POSTs to the running shop-api's /users route.
set -euo pipefail

EMAIL=""
ROLE="admin"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --email)
      EMAIL="$2"
      shift 2
      ;;
    --email=*)
      EMAIL="${1#*=}"
      shift
      ;;
    --role)
      ROLE="$2"
      shift 2
      ;;
    --role=*)
      ROLE="${1#*=}"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$EMAIL" ]]; then
  echo "ERROR: --email is required" >&2
  exit 2
fi

case "$ROLE" in
  admin|user) ;;
  *)
    echo "ERROR: --role must be one of: admin, user" >&2
    exit 2
    ;;
esac

: "${PORT:=18080}"
URL="http://localhost:${PORT}/users"
BODY="{\"email\": \"${EMAIL}\", \"role\": \"${ROLE}\"}"

if command -v curl >/dev/null 2>&1; then
  curl -sf -X POST -H "Content-Type: application/json" -d "$BODY" "$URL" >/dev/null
else
  python3 -c '
import json, sys, urllib.request
url, body = sys.argv[1], sys.argv[2]
req = urllib.request.Request(url, data=body.encode(), headers={"Content-Type": "application/json"}, method="POST")
urllib.request.urlopen(req, timeout=5).read()
' "$URL" "$BODY"
fi

echo "create-test-user: ok (email=${EMAIL}, role=${ROLE})"
