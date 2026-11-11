#!/usr/bin/env bash
# Follows what Marathon clients are doing: prints each event from the
# server's event stream (GET /api/v1/events, Server-Sent Events; PLAN.md
# §8, §9) as one line of JSON, as it happens. Stop with Ctrl-C.
#
#   examples/tail-events.sh [EVENT ...]
#
# With EVENT names (`now_playing`, `channel_changed`, `paused`, `resumed`,
# `item_ended`, `standby`, `wake`, `error`), prints only those. Each line
# looks like:
#
#   {"client_id":"…","ts_ms":1762790400000,"event":"now_playing","channel_id":"office",…}
#
# so it pipes nicely into `jq`, e.g. to print just titles:
#
#   examples/tail-events.sh now_playing | jq -r .title
#
# Environment:
#   MARATHON_SERVER  server base URL (default http://127.0.0.1:7878)
#   MARATHON_TOKEN   bearer token, if the server has one

set -euo pipefail

if [[ ${1:-} == -h || ${1:-} == --help ]]; then
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
    exit 2
fi

server="${MARATHON_SERVER:-http://127.0.0.1:7878}"
wanted=" $* "

auth=()
if [[ -n ${MARATHON_TOKEN:-} ]]; then
    auth=(-H "Authorization: Bearer $MARATHON_TOKEN")
fi

# Each SSE message is an `event:` line naming the event, a `data:` line
# with its JSON, and a blank line. Lines starting with `:` are keep-alive
# comments.
name=""
curl --silent --show-error --fail-with-body --no-buffer \
    ${auth[@]+"${auth[@]}"} \
    -H 'Accept: text/event-stream' \
    "$server/api/v1/events" |
    while IFS= read -r line; do
        line=${line%$'\r'}
        case $line in
            event:*) name=${line#event:}; name=${name# } ;;
            data:*)
                if [[ $# -eq 0 || $wanted == *" $name "* ]]; then
                    data=${line#data:}
                    printf '%s\n' "${data# }"
                fi
                ;;
            "") name="" ;;
        esac
    done
