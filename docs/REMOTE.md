# Remote control and events

Anything you can do with a keyboard or controller, you can also do over
the network, and anything Marathon does can be followed from outside. This
is how Marathon fits into setups nobody planned for: a button on a phone,
a home-automation rule, a script, an AI agent. Each is a small program
that talks to the server's HTTP API (PLAN.md §9 "Input and control").

```text
 your program ── POST /clients/{id}/actions ──► server ──► desktop client
 your program ◄── GET /events ───────────────── server ◄── desktop client
```

The desktop client connects to its server when it starts and stays
connected. If the server restarts or the network drops, the client
reconnects on its own, retrying every half-second at first and then less
often, up to every 30 seconds. Remote control works with `--embedded`
too, but an embedded server listens only on `127.0.0.1`, so it's
reachable only from the same machine.

The examples below assume the server is at `http://127.0.0.1:7878`. If it
has a `token`, add `-H "Authorization: Bearer $TOKEN"` to every request;
the control routes need it like every other route.

## Sending actions

```sh
curl -X POST localhost:7878/api/v1/clients/default/actions -d '{"action":"channel_up"}'
```

`default` means the client that was most recently active: the last one to
connect or report anything (such as changing channel). With one TV, that's
the TV. To pick a client, use its ID from `GET /api/v1/clients`:

```sh
curl -s localhost:7878/api/v1/clients | jq
```

```json
[
  {
    "id": "6f1c…",
    "connected_at_ms": 1762790400000,
    "last_active_ms": 1762790412000,
    "is_default": true,
    "playing": {
      "channel_id": "office",
      "channel_number": 1,
      "item_id": "0190…",
      "title": "The Office · S03E14 · The Injury"
    },
    "paused": false,
    "standby": false
  }
]
```

The answer to a POST is `202 Accepted` with the ID of the client the
action went to. It means the action was handed to that client's
connection, not that the client did anything with it: watch `/events` to
see what happened. `404` means no such client is connected (for
`default`: none at all). `400` means the body isn't a valid action, and
the message says why, for example
``unknown variant `chanel_up`, expected one of `channel_up`, …``.

The body is read as JSON whatever its `Content-Type`, so a bare `curl -d`
works.

### Actions

Every action is a JSON object whose `action` field names it. Some carry
more fields.

| Action | JSON |
|---|---|
| Channel up / down | `{"action":"channel_up"}`, `{"action":"channel_down"}` |
| Go to channel 4 | `{"action":"go_to_channel","number":4}` |
| Go to a channel by its config ID | `{"action":"go_to_channel_id","id":"office"}` |
| Pause or resume | `{"action":"toggle_pause"}` |
| Resume / pause (no toggling) | `{"action":"play"}`, `{"action":"pause"}` |
| Skip 30 seconds back | `{"action":"seek_relative","seconds":-30}` |
| Next / previous episode | `{"action":"next_item"}`, `{"action":"previous_item"}` |
| Arrow keys | `{"action":"navigate","direction":"up"}` (`down`, `left`, `right`) |
| Enter | `{"action":"select"}` |
| Esc | `{"action":"back"}` |
| Show what's on | `{"action":"show_info"}` |
| Open or close the guide | `{"action":"toggle_guide"}` |
| Subtitles on / off | `{"action":"toggle_subtitles"}` |
| Fullscreen on / off | `{"action":"toggle_fullscreen"}` |
| Quit | `{"action":"quit"}` |

`navigate`, `select`, and `back` behave exactly like the arrow keys,
Enter, and Esc ([CONTROLS.md](CONTROLS.md)): with the guide open, `up` and
`down` move its highlight; otherwise they change channel. `standby`
pauses and saves the position (as when the TV switches to another input),
and `wake` resumes, but only if `standby` was what paused it.

Actions from every source are equal and are handled in the order they
arrive. A remote can't lock out the keyboard or vice versa.

## Following events

`GET /api/v1/events` is a [Server-Sent Events][sse] stream of everything
connected clients report, as it happens:

```sh
curl -N localhost:7878/api/v1/events
```

```text
event: channel_changed
data: {"client_id":"6f1c…","ts_ms":1762790412000,"event":"channel_changed","channel_id":"office","channel_number":1,"channel_name":"The Office"}

event: now_playing
data: {"client_id":"6f1c…","ts_ms":1762790412150,"event":"now_playing","channel_id":"office","channel_number":1,"item_id":"0190…","title":"The Office · S03E14 · The Injury","position_ms":735000}
```

Each message's `event:` line names the event; its `data:` is the event as
JSON, with the ID of the client it came from and when the server got it
(milliseconds since 1970).

| Event | When | Fields |
|---|---|---|
| `channel_changed` | The client tuned to a channel | `channel_id`, `channel_number`, `channel_name` |
| `now_playing` | An item started: after tuning, skipping, or the previous one ending | `channel_id`, `channel_number`, `item_id`, `title`, `position_ms` (where it started) |
| `paused` / `resumed` | Playback paused or resumed | `channel_id`, `item_id`, `position_ms` |
| `item_ended` | An item played to the end | `channel_id`, `item_id` |
| `standby` / `wake` | The client went into or out of standby | |
| `error` | Something went wrong, e.g. a file wouldn't play (playback carries on) | `message` |

The stream only has events from the moment you connect. When a client
reconnects (after a server restart, say), it re-sends `now_playing` (and
`paused` or `standby` if that's its state), so the stream and
`GET /clients` catch up. Lines starting with `:` are keep-alive comments
sent every 15 seconds; ignore them. A subscriber that reads too slowly
gets a comment saying how many events it missed.

## Example scripts

[`examples/`](../examples) has two shell scripts (needing only `bash`
and `curl`) that do the above with less typing. Both read
`MARATHON_SERVER` (default `http://127.0.0.1:7878`) and `MARATHON_TOKEN`.

```sh
examples/send-action.sh channel_up
examples/send-action.sh go_to_channel number=4
examples/send-action.sh seek_relative seconds=-30
MARATHON_CLIENT=6f1c… examples/send-action.sh toggle_pause

examples/tail-events.sh                          # everything, one JSON object per line
examples/tail-events.sh now_playing | jq -r .title
```

## For client authors

A client's control channel is a WebSocket at `GET /api/v1/clients/connect`,
opened with the client's `X-Marathon-Client` header (and the token, if
any). The server sends each relayed action as a JSON text message, shaped
as above. The client sends each of its events the same way, shaped like
the `data:` above minus `client_id` and `ts_ms`. A client that connects
again replaces its earlier connection. The full schemas (`Action`,
`Event`, `RelayedEvent`, `ClientInfo`) are in the OpenAPI document at
`/api/v1/openapi.json`. `marathon_client::remote` is the Rust
implementation.

[sse]: https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events
