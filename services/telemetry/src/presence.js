// The privacy-relevant part of the telemetry service, kept free of Workers APIs so it can be tested on its
// own (test/presence.test.mjs). docs/telemetry.md says what is collected; this is where that holds.

/** How big an area a dot stands for: a mothership is placed on a grid of cells about this wide. */
export const CELL_KM = 25;

/** A mothership counts as online for this long after its last heartbeat. Heartbeats come every 5 minutes. */
export const ONLINE_SECONDS = 12 * 60;

/** Rows older than this are deleted (worker.js prunes them): the service keeps no history. */
export const RETAIN_SECONDS = 60 * 60;

/** Seconds a mothership should wait before its next heartbeat. */
export const NEXT_IN_SECONDS = 5 * 60;

const PLATFORMS = new Set(['linux-x86_64', 'darwin-arm64', 'other']);
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const VERSION = /^[0-9A-Za-z.+-]{1,32}$/;

/**
 * Validates a heartbeat body. Returns the fields to store, or an error string. Anything not listed here is
 * ignored, so a client can never make the service keep more than this.
 */
export function parseHeartbeat(body) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return { error: 'body must be a JSON object' };
  const { install_id: installId, version, platform, colonies, online = true } = body;
  if (typeof installId !== 'string' || !UUID.test(installId)) return { error: 'install_id must be a random (v4) UUID' };
  if (typeof online !== 'boolean') return { error: 'online must be a boolean' };
  if (!online) return { installId, online: false };
  if (typeof version !== 'string' || !VERSION.test(version)) return { error: 'version is not a version' };
  if (!PLATFORMS.has(platform)) return { error: `platform must be one of ${[...PLATFORMS].join(', ')}` };
  if (!Number.isInteger(colonies) || colonies < 0 || colonies > 64) return { error: 'colonies must be an integer from 0 to 64' };
  return { installId, online: true, version, platform, colonies };
}

/**
 * The centre of the grid cell a location falls in, rounded to 3 decimals. Cells are CELL_KM tall, and as
 * wide as that at their own latitude, so a dot never says more than "somewhere in this area". The input is
 * Cloudflare's estimate from the connecting IP, which is itself only city-level.
 */
export function cellOf(latitude, longitude) {
  const lat = Number(latitude);
  const lon = Number(longitude);
  if (!Number.isFinite(lat) || !Number.isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) return null;
  const latStep = CELL_KM / 111.32;
  const cellLat = Math.min(90, (Math.floor(lat / latStep) + 0.5) * latStep);
  const lonStep = Math.min(360, latStep / Math.max(Math.cos((cellLat * Math.PI) / 180), 0.05));
  let cellLon = (Math.floor((lon + 180) / lonStep) + 0.5) * lonStep - 180;
  if (cellLon > 180) cellLon -= 360;
  return { lat: round3(cellLat), lon: round3(cellLon) };
}

function round3(value) {
  return Math.round(value * 1000) / 1000;
}

/** The stored key for an install: a hash, so the id a mothership sends is never kept as such. */
export async function installKey(installId) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`colonizer-telemetry:${installId}`));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** The public view: counts per cell, never per install. */
export function presenceView(rows, now) {
  const cells = rows
    .filter((row) => row.cell_lat !== null && row.cell_lon !== null)
    .map((row) => ({ lat: row.cell_lat, lon: row.cell_lon, motherships: row.motherships, colonies: row.colonies }));
  return {
    updated_at: new Date(now * 1000).toISOString(),
    online_window_seconds: ONLINE_SECONDS,
    cell_km: CELL_KM,
    motherships: rows.reduce((sum, row) => sum + row.motherships, 0),
    colonies: rows.reduce((sum, row) => sum + row.colonies, 0),
    cells,
  };
}
