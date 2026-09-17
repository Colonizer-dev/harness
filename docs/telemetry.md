# The live map

[colonizer.dev/live](https://colonizer.dev/live) is a world map with a dot for every area where a
mothership is online, which lights up while colonies run there. It shows only motherships whose user
switched it on.

**It is off until you switch it on.** After GitHub and Claude are connected, the web UI asks once. You
can change your answer at any time in Settings, under Live map. Until you answer, and whenever it is off,
the mothership sends nothing.

## What is sent

While it is on, the mothership sends a heartbeat to `https://telemetry.colonizer.dev/v1/heartbeat` every
5 minutes, and within a minute when the number of running colonies changes. The heartbeat is this, and
nothing else:

```json
{
  "install_id": "0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b",
  "version": "0.1.3",
  "platform": "darwin-arm64",
  "colonies": 2
}
```

| Field | What it is |
| :--- | :--- |
| `install_id` | A random UUID, created when you switch the live map on. It lets a mothership's heartbeats count once. |
| `version` | The Colonizer version. |
| `platform` | `linux-x86_64`, `darwin-arm64`, or `other`. |
| `colonies` | How many colonies have a running microVM, capped at 64. |

No repository, issue, branch, code, prompt, user name, host name or path is sent. Settings shows the
exact heartbeat the mothership will send next.

When you switch it off, the mothership sends `{"install_id": "…", "online": false}`, which removes its
row straight away, and then forgets the id. If you switch it on again later, it gets a new id, so the two
periods can't be linked. Stopping the mothership sends the same message, but keeps the id.

## What the service keeps

The service is a Cloudflare Worker with a D1 database. Its source is
[services/telemetry](https://github.com/Colonizer-dev/harness/tree/main/services/telemetry). It keeps one row per mothership:

| Column | What it is |
| :--- | :--- |
| `install` | SHA-256 of the `install_id`, not the id itself. |
| `cell_lat`, `cell_lon` | The centre of a grid cell about 25 km across. |
| `colonies`, `version`, `platform` | From the last heartbeat. |
| `seen_at` | When the last heartbeat arrived. |

The location comes from Cloudflare's own estimate for the connection, which is based on the IP address
and is roughly city-level. The service snaps that estimate to its 25 km cell, and stores only the cell.
The IP address itself is never stored. It is used, in memory, only to limit each address to 30 requests
a minute. Two motherships in the same cell are the same dot.

A mothership counts as online for 12 minutes after its last heartbeat. Rows older than an hour are
deleted, so the service keeps no history. Cloudflare's own request logs for the Worker are separate from
this table.

## What is public

`GET https://telemetry.colonizer.dev/v1/presence` is what the map reads, and anyone can read it:

```json
{
  "updated_at": "2026-09-17T13:10:00.000Z",
  "online_window_seconds": 720,
  "cell_km": 25,
  "motherships": 3,
  "colonies": 4,
  "cells": [{"lat": 52.377, "lon": 4.913, "motherships": 2, "colonies": 3}]
}
```

It gives counts per cell, never per mothership. Motherships the service couldn't place count towards the
totals but have no cell.

## Keeping it off for good

Set `DO_NOT_TRACK=1` or `COLONIZER_TELEMETRY=off` in the mothership's environment. The live map then
stays off, whatever Settings says, and the web UI doesn't ask. `COLONIZER_TELEMETRY_URL` points the
heartbeat at another receiver, such as your own deployment of the service.

## Running the service

From `services/telemetry`:

```sh
node --test                                                 # the privacy-relevant logic
npx wrangler d1 migrations apply colonizer-telemetry --remote
npx wrangler deploy
```

Old rows are pruned during heartbeats, at most once every 10 minutes, so the service needs no cron
trigger.
