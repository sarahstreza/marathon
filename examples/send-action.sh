#!/usr/bin/env bash
# Sends an action to a Marathon client through the server, as if pressed on
# its remote (POST /api/v1/clients/{id}/actions; PLAN.md §8, §9).
#
#   examples/send-action.sh ACTION [FIELD=VALUE ...]
#
# ACTION is an action name from the API (`channel_up`, `toggle_pause`,
# `go_to_channel`, ...; the full list is the `Action` schema in
# /api/v1/openapi.json). Actions with data take it as FIELD=VALUE pairs;
# values made of digits (with an optional minus sign) are sent as numbers,
# anything else as a string. For example:
#
#   examples/send-action.sh channel_up
#   examples/send-action.sh go_to_channel number=4
#   examples/send-action.sh go_to_channel_id id=office
#   examples/send-action.sh seek_relative seconds=-30
#   examples/send-action.sh navigate direction=down
#
# Environment:
#   MARATHON_SERVER  server base URL (default http://127.0.0.1:7878)
#   MARATHON_TOKEN   bearer token, if the server has one
#   MARATHON_CLIENT  which client (default `default`: the most recently
#                    active one; see GET /api/v1/clients for the others)
#
# Prints the server's answer: {"client_id": ...} when the action was
# handed over, or an error. Exits non-zero on an error.

set -euo pipefail

if [[ $# -lt 1 || $1 == -h || $1 == --help ]]; then
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit 2
fi

server="${MARATHON_SERVER:-http://127.0.0.1:7878}"
client="${MARATHON_CLIENT:-default}"

# Escapes a string for use inside JSON double quotes.
json_string() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//\"/\\\"}
    printf '"%s"' "$s"
}

body="{\"action\":$(json_string "$1")"
shift
for pair in "$@"; do
    if [[ $pair != *=* ]]; then
        echo "error: expected FIELD=VALUE, got '$pair'" >&2
        exit 2
    fi
    field=${pair%%=*}
    value=${pair#*=}
    if [[ $value =~ ^-?[0-9]+$ ]]; then
        body+=",$(json_string "$field"):$value"
    else
        body+=",$(json_string "$field"):$(json_string "$value")"
    fi
done
body+="}"

auth=()
if [[ -n ${MARATHON_TOKEN:-} ]]; then
    auth=(-H "Authorization: Bearer $MARATHON_TOKEN")
fi

response=$(curl --silent --show-error \
    ${auth[@]+"${auth[@]}"} \
    -H 'Content-Type: application/json' \
    --data "$body" \
    --write-out '\n%{http_code}' \
    "$server/api/v1/clients/$client/actions")
status=${response##*$'\n'}
printf '%s\n' "${response%$'\n'*}"
[[ $status == 2* ]]
