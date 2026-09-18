# The live map

[colonizer.dev/live](https://colonizer.dev/live) is a world map with a dot for every area where a
mothership is online, which lights up while colonies run there. It shows only motherships whose user
switched it on.

**It is off until you switch it on.** The first-run Setup checklist ends with the question, on its live
map row: switch it on there, or leave it off. If Setup has already been shown, the web UI asks once with
a small prompt instead. You can change your answer at any time in Settings, under Live map. Until you
answer, and whenever it is off, the mothership sends nothing.

**This is not [usage data](usage-data.md).** That is a second, separate thing: a batch of counts about
how the harness is used, with its own switch and a different random id, and on by default, though in
its current release it is never sent at all, only built, shown and kept. The environment switches below
keep both off.

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
row straight away, and then forgets the id. The id is forgotten whether or not that message gets
through. If the service is unreachable, the row simply stays: it stops being counted 12 minutes after
the last heartbeat, and because the id is gone, nothing can refresh it. If you switch it on again
later, it gets a new id, so the two periods can't be linked. Stopping the mothership sends the same
message, but keeps the id.

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

A cell is only as anonymous as the number of motherships in it. Where a cell holds one mothership, the
dot is that one install, placed to within about 25 km, and most cells will hold one.

A row stops being counted 12 minutes after its last heartbeat. That part is unconditional: every read
filters on `seen_at`, so nothing older is ever served or counted. Deleting the row is a different
matter. A prune removes rows more than an hour old, and it runs only on the back of a request to one of
the two routes, a heartbeat or a read of the map, at most once every 10 minutes per isolate. A row can
therefore outlive the hour by up to those ten minutes, and if nothing at all reaches the service,
nothing is deleted: the row sits there, read by nothing, until the next request arrives. There is no
scheduled prune; the section below says why.

Cloudflare's own request logs for the Worker are outside this table and outside the project's control.
They follow whatever retention the Cloudflare account has, which has not been checked, and unlike this
table they do see IP addresses.

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

The counts are a floor, not a measurement: they count motherships that switched the live map on and
whose heartbeat arrived in the last 12 minutes. They are not installs, not users, and not adoption
statistics.

## Keeping it off for good

Set `DO_NOT_TRACK=1` or `COLONIZER_TELEMETRY=off` in the mothership's environment. The live map then
stays off, whatever Settings says, and the web UI doesn't ask. `COLONIZER_TELEMETRY_URL` points the
heartbeat at another receiver, such as your own deployment of the service.

Both switches keep [usage data](usage-data.md) off too; that page also names one more switch,
`CI=true`, which this map ignores.

## Running the service

From `services/telemetry`:

```sh
node --test                                                 # the privacy-relevant logic
npx wrangler d1 migrations apply colonizer-telemetry --remote
npx wrangler deploy
```

The prune rides on requests to either route, at most once every 10 minutes per isolate. There is no
cron trigger, because one needs a workers.dev subdomain on the account, which it doesn't have (deploy
error 10063).
