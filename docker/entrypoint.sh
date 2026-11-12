#!/bin/sh
# Entrypoint for the marathond image: runs the server as PUID:PGID
# (default 1000:1000), so files it writes under /data belong to a chosen
# host user.
#
# Started as root (the default), it makes /data owned by PUID:PGID and then
# drops to that uid/gid with su-exec, which execs rather than staying as a
# parent process, so marathond is PID 1 and receives signals directly.
# Started as another user (`docker run --user`), it cannot switch users, so
# it runs marathond as that user and ignores PUID/PGID.
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"

for pair in "PUID=$PUID" "PGID=$PGID"; do
    case "${pair#*=}" in
        '' | *[!0-9]*)
            echo "entrypoint: ${pair%%=*} must be a numeric id, got '${pair#*=}'" >&2
            exit 64
            ;;
    esac
done

if [ "$(id -u)" != 0 ]; then
    exec /usr/local/bin/marathond "$@"
fi

# Re-own /data only when its top level is wrong (a fresh volume, or PUID or
# PGID changed since the last start), so a normal restart does no work. A
# failure (say, a bind mount on NFS with root squashing) is not fatal:
# marathond reports a clearer error if it really cannot write.
if [ "$(stat -c %u:%g /data)" != "$PUID:$PGID" ]; then
    chown -R "$PUID:$PGID" /data \
        || echo "entrypoint: could not chown /data to $PUID:$PGID; continuing" >&2
fi

exec su-exec "$PUID:$PGID" /usr/local/bin/marathond "$@"
