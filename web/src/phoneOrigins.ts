// Which origin a phone should open, out of the mothership's preference-ordered list (relay →
// tailnet → lan). Pure, so both the pairing pane's QR code (PhonePane) and the "on your network"
// address on the Your cockpit card (cockpitAddress) pick the same one instead of each deciding.
import type { PhoneOrigin } from "./types";

/** The first reachable origin, else the first origin at all (so a pane can still explain itself). */
export function chosenOrigin(origins: readonly PhoneOrigin[]): PhoneOrigin | null {
  return origins.find((origin) => origin.reachable) ?? origins[0] ?? null;
}
