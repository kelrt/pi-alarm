# pi-alarm BLE protocol, version 1

How the phone app talks to the Pi daemon. The daemon side is
`src/protocol.rs` (messages, framing) and `src/ble.rs` (Bluetooth); the app
side is `app/lib/ble/`.

Every `json request` example in this file is run against the real request
handler by a test (`tests/protocol_doc.rs`), so the examples are known to be
accepted.

## 1. Bluetooth layout

The Pi is a BLE peripheral with one GATT service. The phone connects and
writes; nothing is carried in advertising data except the service UUID and
the device name.

| What | UUID | Properties |
|---|---|---|
| Service | `7a1e0001-5c9b-4f6a-9d2e-b1a2c3d4e5f6` | primary |
| Request | `7a1e0002-5c9b-4f6a-9d2e-b1a2c3d4e5f6` | write (with response) |
| Response | `7a1e0003-5c9b-4f6a-9d2e-b1a2c3d4e5f6` | notify |
| Info | `7a1e0004-5c9b-4f6a-9d2e-b1a2c3d4e5f6` | read |

- **Request**: the phone writes request chunks here.
- **Response**: the Pi sends response chunks here as notifications.
- **Info**: a short JSON text, `{"v":1,"name":"pi-alarm","version":"0.1.0"}`.
  Not needed by the app; useful to confirm you found the right device with a
  generic BLE tool.

There is no pairing, bonding or encryption in version 1 (DECISIONS.md, D22).
One phone at a time.

## 2. Connection sequence

What the app does every time it connects:

1. Connect to the remembered Pi (found the first time by scanning for the
   service UUID).
2. Discover services, subscribe to **Response** notifications.
3. Send `set_time`. Always first: the Pi has no clock of its own.
4. Send `status` to learn the available actions.
5. Send any queued `put`/`delete` changes, then `list`.

A request must not be sent before step 2 is complete: the Pi would handle it
but could not deliver the response.

## 3. Framing

A BLE write or notification carries at most `MTU - 3` bytes, which can be as
little as 20. Messages are therefore split into **chunks**. Each BLE write
(phone to Pi) and each notification (Pi to phone) is exactly one chunk.

```
byte 0      header
bytes 1..   a piece of the message
```

Header bits:

| Bit | Value | Meaning |
|---|---|---|
| 0 | `0x01` | first chunk of a message |
| 1 | `0x02` | last chunk of a message |
| 2-7 | | must be 0 |

So a message that fits in one chunk has header `0x03`; a three-chunk message
has headers `0x01`, `0x00`, `0x02`.

Rules:

- The sender uses chunks of at most `MTU - 3` bytes, never more than 512
  and never fewer than 20. The Pi learns the MTU from the phone's writes and
  uses the same size for its notifications.
- A chunk with the "first" bit always starts a new message. If an earlier
  message was left unfinished (the phone went out of range), it is dropped.
- A chunk without the "first" bit, when no message is in progress, is
  ignored.
- A complete message is at most 16384 bytes. A longer one is dropped.
- BLE "long writes" (writes with an offset) are not supported; do the
  chunking described here instead.

Example: the 5-byte message `hello` with a 20-byte chunk size is the single
chunk `03 68 65 6c 6c 6f`. A 50-byte message becomes three chunks of 20, 20
and 13 bytes with headers `01`, `00`, `02`.

## 4. Messages

A message is one JSON object encoded as UTF-8. The phone sends a **request**;
the Pi answers with exactly one **response**. Requests are handled one at a
time, in order. The phone should wait for a response before sending the next
request.

### Request

| Field | Type | |
|---|---|---|
| `v` | number | protocol version, `1` |
| `id` | number | chosen by the phone, copied into the response. Optional, default 0 |
| `op` | string | the operation, see below |
| ... | | the operation's own fields |

Unknown fields are ignored.

### Response

| Field | Type | |
|---|---|---|
| `v` | number | `1` |
| `id` | number | the request's `id` |
| `ok` | boolean | `true` on success |
| ... | | on success: the operation's result fields |
| `error` | object | on failure: `{"code": "...", "message": "..."}` |

### Versioning

- A request with a `v` the daemon does not speak gets `unsupported_version`.
- New optional fields and new operations may be added without changing `v`;
  both sides ignore fields they do not know.
- Anything that changes the meaning of existing fields increases `v`.

## 5. Operations

### `status`

What the Pi is and what it can do.

```json request
{"v": 1, "id": 1, "op": "status"}
```

```json
{
  "v": 1, "id": 1, "ok": true,
  "name": "pi-alarm",
  "version": "0.1.0",
  "time": {
    "unix_ms": 1794333600000,
    "tz": "America/Chicago",
    "utc_offset_min": -360,
    "synced": true
  },
  "actions": [
    {"name": "fan_3", "label": "Fan 3"},
    {"name": "fan_off", "label": "Fan off"},
    {"name": "light_toggle", "label": "Light"}
  ],
  "alarm_count": 1,
  "next": {"alarm_id": "a1b2c3d4", "action": "light_toggle", "unix_ms": 1794399600000}
}
```

- `time.unix_ms`: the Pi's clock, milliseconds since 1970-01-01 UTC.
- `time.tz`, `time.utc_offset_min`: the zone alarm times are interpreted in,
  and its current offset from UTC.
- `time.synced`: `true` once NTP or a `set_time` has confirmed the clock
  since the daemon started. While `false`, alarms follow a clock that may be
  wrong.
- `actions`: what can be used in alarm steps and in `action`.
- `next`: the next step that will run, or `null`.

### `set_time`

Tell the Pi the current time.

```json request
{"v": 1, "id": 2, "op": "set_time", "unix_ms": 1794333600000, "utc_offset_min": -360}
```

| Field | Type | |
|---|---|---|
| `unix_ms` | number | the phone's time, milliseconds since 1970-01-01 UTC |
| `utc_offset_min` | number | optional: the phone's current offset from UTC, minutes |

```json
{
  "v": 1, "id": 2, "ok": true,
  "adjusted": true,
  "delta_ms": 450000,
  "tz_match": true,
  "time": {"unix_ms": 1794333600000, "tz": "America/Chicago", "utc_offset_min": -360, "synced": true}
}
```

- The clock is changed only if it differs from the phone's by 2000 ms or
  more. `adjusted` says whether it was; `delta_ms` is phone time minus Pi
  time before any change.
- `tz_match` is present only if the request had `utc_offset_min`. `false`
  means the Pi's time zone differs from the phone's, so alarms would fire at
  an unexpected hour. Fix the Pi's zone (`timezone` in its config, or
  `sudo timedatectl set-timezone ...`).
- Times before 2024 or after 2099 are refused with `bad_request`.
- Fails with `clock_set_failed` if the daemon may not set the clock.

### `list`

```json request
{"v": 1, "id": 3, "op": "list"}
```

```json
{"v": 1, "id": 3, "ok": true, "alarms": [ ...alarm objects... ]}
```

### `put`

Create an alarm, or replace the one with the same `id`.

```json request
{
  "v": 1, "id": 4, "op": "put",
  "alarm": {
    "id": "a1b2c3d4",
    "label": "Work",
    "hour": 6,
    "minute": 30,
    "days": ["mon", "tue", "wed", "thu", "fri"],
    "enabled": true,
    "steps": [
      {"offset_s": -600, "action": "light_toggle"},
      {"offset_s": 0, "action": "fan_3"}
    ]
  }
}
```

```json
{"v": 1, "id": 4, "ok": true, "created": true, "alarm": { ...the alarm as stored... }}
```

The alarm object:

| Field | Type | Rules |
|---|---|---|
| `id` | string | 1-24 characters from `a-z 0-9 - _`. The phone picks it (random) |
| `label` | string | optional, at most 40 characters |
| `hour` | number | 0-23, wall-clock time in the Pi's time zone |
| `minute` | number | 0-59 |
| `days` | array | at least one of `"mon" "tue" "wed" "thu" "fri" "sat" "sun"` |
| `enabled` | boolean | |
| `steps` | array | 1-8 steps |
| `steps[].offset_s` | number | seconds relative to the alarm time, -21600 to 21600 |
| `steps[].action` | string | an action name from `status` |

- The Pi stores the alarm in a tidy form (days Monday-first without
  duplicates, steps sorted by offset) and returns that form in `alarm`.
- `days` refers to the day of the alarm time. A Monday 00:05 alarm with a
  step at -600 runs that step on Sunday at 23:55.
- Sending the same `put` twice has the same result as sending it once, so it
  is safe to retry.
- Errors: `invalid_alarm` (a rule above is broken, or an unknown action),
  `too_many_alarms` (20 is the limit).

### `delete`

```json request
{"v": 1, "id": 5, "op": "delete", "alarm_id": "a1b2c3d4"}
```

```json
{"v": 1, "id": 5, "ok": true, "deleted": true}
```

`deleted` is `false` if there was no such alarm. That is still a success, so
a retried delete is harmless.

### `action`

Transmit an action now.

```json request
{"v": 1, "id": 6, "op": "action", "name": "fan_3"}
```

```json
{"v": 1, "id": 6, "ok": true, "name": "fan_3"}
```

The response is sent after the transmission has finished (about 0.3 s).
Errors: `unknown_action`, `transmit_failed`.

## 6. Error codes

| Code | Meaning |
|---|---|
| `bad_json` | the message is not valid JSON |
| `bad_request` | a required field is missing or has the wrong type |
| `unsupported_version` | `v` is not 1 |
| `unknown_op` | `op` is not one of the operations above |
| `invalid_alarm` | the alarm breaks a rule; `message` says which |
| `too_many_alarms` | the alarm list is full |
| `unknown_action` | no action with that name is configured |
| `transmit_failed` | the radio reported an error |
| `clock_set_failed` | the daemon is not allowed to set the clock |
| `storage_error` | the alarms file could not be read or written |

An error response looks like this:

```json
{"v": 1, "id": 6, "ok": false, "error": {"code": "unknown_action", "message": "unknown action `fan_1`"}}
```

Retrying helps for none of these except possibly `transmit_failed` and
`storage_error`. The app drops a queued change that gets any error response
and shows the message.

## 7. Trying it without the phone

On the Pi or on a development machine, one request can be run through the
same handler the BLE server uses:

```sh
pi-alarm request '{"v":1,"id":1,"op":"status"}'
pi-alarm --mock-radio request '{"v":1,"op":"action","name":"fan_3"}'
```

With a generic BLE app (for example nRF Connect): connect to `pi-alarm`,
read **Info**, enable notifications on **Response**, then write
`03` followed by the request's bytes to **Request**. A short request such as
`{"v":1,"op":"list"}` fits in one chunk once the app has negotiated a larger
MTU (most do on connect).
