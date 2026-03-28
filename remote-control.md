# Remote Control Protocol

The firmware can open a reverse WebSocket connection to a remote control server. This lets a public server control a device that sits behind NAT, while the local web UI keeps working.

## Transport

- Device setting: `remote_control_enabled`
- Device setting: `remote_control_url`
- Device setting: `remote_control_validate_cert`
- Supported URL schemes: `ws://` and `wss://`
- For `wss://`, certificate validation uses the ESP-IDF CA bundle when `remote_control_validate_cert=true`
- Self-signed or private-CA endpoints are not supported with validation enabled; disable validation for those endpoints

## Connection lifecycle

On connect, the device immediately sends:

1. `state`
2. `event_log_state`

After that, the device forwards the same real-time state updates used by the local UI:

- `state`
- `event_log_event`
- `pong`
- `error`

The device also sends application-level `ping` messages every 5 seconds and expects a matching `pong` within 15 seconds. Missing pongs force a reconnect.

Reconnect policy:

- Initial delay: 1 second
- Exponential backoff up to 30 seconds
- Any remote-control settings change resets the backoff and reconnects immediately

## Server -> Device commands

These messages are JSON objects with a `type` field.

### Ping

```json
{ "type": "ping", "nonce": 123 }
```

Device reply:

```json
{
  "type": "pong",
  "nonce": 123,
  "server_uptime_s": 456,
  "free_heap_bytes": 123456,
  "connected_clients": 2,
  "client_ips": ["192.168.1.10", "192.168.1.11"]
}
```

The remote server must also reply to device-originated pings:

```json
{ "type": "pong", "nonce": 123 }
```

Extra fields are allowed and ignored.

### Collar list management

Add:

```json
{ "type": "add_collar", "name": "Rex", "collar_id": 39802, "channel": 0 }
```

Update:

```json
{
  "type": "update_collar",
  "original_name": "Rex",
  "name": "Rex",
  "collar_id": 39802,
  "channel": 1
}
```

Delete:

```json
{ "type": "delete_collar", "name": "Rex" }
```

### Preset management

Save or replace:

```json
{
  "type": "save_preset",
  "original_name": null,
  "preset": {
    "name": "Night Walk",
    "tracks": [
      {
        "collar_name": "Rex",
        "steps": [
          { "mode": "vibrate", "intensity": 40, "duration_ms": 1500 },
          { "mode": "pause", "intensity": 0, "duration_ms": 500 },
          { "mode": "beep", "intensity": 0, "duration_ms": 500 }
        ]
      }
    ]
  }
}
```

Delete:

```json
{ "type": "delete_preset", "name": "Night Walk" }
```

Reorder:

```json
{ "type": "reorder_presets", "names": ["Night Walk", "Recall"] }
```

### Preset execution

Run:

```json
{ "type": "run_preset", "name": "Night Walk" }
```

Stop current preset:

```json
{ "type": "stop_preset" }
```

Emergency stop:

```json
{ "type": "stop_all" }
```

`stop_all` also cancels active held actions and applies the normal RF lockout.

### Individual actions

Timed action:

```json
{
  "type": "run_action",
  "collar_name": "Rex",
  "mode": "shock",
  "intensity": 25,
  "duration_ms": 1500
}
```

Held action start:

```json
{
  "type": "start_action",
  "collar_name": "Rex",
  "mode": "vibrate",
  "intensity": 40
}
```

Held action stop:

```json
{
  "type": "stop_action",
  "collar_name": "Rex",
  "mode": "vibrate"
}
```

Held actions are connection-owned. If the remote control socket disconnects, the firmware cancels any active held action started over that socket.

## Device -> Server messages

### State

Sent on connect and whenever collars, presets, the running preset, or STOP lockout changes.

```json
{
  "type": "state",
  "app_version": "0.1.0+...",
  "server_uptime_s": 123,
  "collars": [],
  "presets": [],
  "preset_running": null,
  "rf_lockout_remaining_ms": 0
}
```

### Event log snapshot

Sent on connect and whenever logging is toggled.

```json
{
  "type": "event_log_state",
  "enabled": true,
  "events": []
}
```

### Event log append

Sent for each new recorded event.

```json
{
  "type": "event_log_event",
  "event": {
    "sequence": 12,
    "monotonic_ms": 456789,
    "unix_ms": 1743151234567,
    "source": "remote_control",
    "event": "action",
    "collar_name": "Rex",
    "mode": "shock",
    "intensity": 25,
    "duration_ms": 1500
  }
}
```

Recorded event kinds:

- `action`
- `preset_run`
- `ntp_sync`
- `remote_control_connection`

### Error

```json
{ "type": "error", "message": "Unknown collar: Rex" }
```

## Unsupported over remote control

These local-UI-only commands are rejected with `error`:

- `start_rf_debug`
- `stop_rf_debug`
- `clear_rf_debug`
- `reboot`
- `get_device_settings`
- `save_device_settings`
- `preview_preset`
- `export`
- `import`
