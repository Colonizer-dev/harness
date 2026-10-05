// Pages the relay serves to a browser itself, without touching the tunnel: the offline page a dead
// tunnel gets, the pairing code a signing-in owner carries to their cockpit, the "Pair this device"
// page every unpaired browser gets on a pair-code install (#1086), its 429 twin, and the 403/404 pair.

const SHELL = (title, main) => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${title}</title>
<style>
  body { font-family: system-ui, sans-serif; background: #101418; color: #e8e6e3;
         display: grid; place-items: center; min-height: 100vh; margin: 0; }
  main { max-width: 34rem; padding: 2rem; text-align: center; }
  h1 { font-size: 1.4rem; }
  p { line-height: 1.6; color: #a8a29e; }
  code { color: #e8e6e3; }
  a { color: #7dd3fc; }
</style>
</head>
<body>
<main>
${main}
</main>
</body>
</html>`;

const OFFLINE = SHELL(
  'Colonizer — cockpit offline',
  `<h1>This cockpit is not connected right now</h1>
  <p>Start Colonizer on the machine with remote access turned on, then reload.</p>`,
);

function respond(html, status) {
  return new Response(html, { status, headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' } });
}

/** 502 for a browser whose install has no live tunnel. no-store: the moment a tunnel connects, reload works. */
export function offlinePage() {
  return new Response(OFFLINE, {
    status: 502,
    headers: { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' },
  });
}

/** 404 for a browser whose subdomain does not name a known install. */
export function unknownInstallPage() {
  return respond(
    SHELL(
      'Colonizer — unknown install',
      `<h1>Nothing is served at this address</h1>
      <p>This address is not a Colonizer install. Check the relay host the mothership printed when remote
      access was turned on.</p>`,
    ),
    404,
  );
}

/** 403 for a sign-in from a GitHub account that is not this install's owner. */
export function forbiddenPage() {
  return respond(
    SHELL(
      'Colonizer — not your cockpit',
      `<h1>This install is owned by another GitHub account</h1>
      <p>Only the owner can open it here. If the install is yours, confirm a fresh pairing code in the
      local cockpit under Settings → Remote access.</p>`,
    ),
    403,
  );
}

/** The pairing code an unbound install shows its signing-in owner, with the way back to finish the flow
 * once the local cockpit has confirmed the code. no-store: the code is a short-lived secret. */
export function pairingPage(code, next) {
  return respond(
    SHELL(
      'Colonizer — pair this browser',
      `<h1>Pair this browser with your cockpit</h1>
      <p>Your code is</p>
      <p><code class="pairing-code" style="font-size: 2rem; letter-spacing: 0.2em;">${code}</code></p>
      <p>Confirm this code in your local cockpit under Settings → Remote access within 10 minutes.</p>
      <p><a href="/_auth?next=${encodeURIComponent(next)}">Continue</a></p>`,
    ),
    200,
  );
}

/** The page a browser gets on a pair-code install (#1086) when it carries no invite and no credential,
 * or a credential the mothership turned down: how to pair it, from the mothership's own screen. Nothing
 * here came from the tunnel, and nothing about the install is said beyond that it exists. 401, so a
 * script reads it as "not signed in"; no-store, so pairing and reloading shows the cockpit. */
export function pairDevicePage({ expired = false } = {}) {
  return respond(
    SHELL(
      'Colonizer — pair this device',
      `<h1>Pair this device</h1>
      ${expired ? '<p>That sign-in has ended or the link was already used, so this device has to be paired again.</p>' : ''}
      <p>On the computer running Colonizer, open the cockpit and go to <strong>Settings → Remote access →
      Sign in on another device</strong>. Open the one-time link it shows on this device (or scan its QR
      code), then type the six digits this device shows into the cockpit there.</p>
      <p>The link works once, for five minutes. Nothing reaches the cockpit until the code is confirmed on
      that computer.</p>`,
    ),
    401,
  );
}

/** 429 from the relay's own throttle (src/throttle.js): too many invite opens or rejected credentials
 * from this client or for this install. Nothing was forwarded. */
export function tooManyAttemptsPage(retryAfterSeconds) {
  const page = respond(
    SHELL(
      'Colonizer — slow down',
      `<h1>Too many attempts</h1>
      <p>This device, or this link, has tried to pair or sign in too many times. Wait a few minutes, then
      open a fresh one-time link from Settings → Remote access on the computer running Colonizer.</p>`,
    ),
    429,
  );
  page.headers.set('retry-after', String(retryAfterSeconds));
  return page;
}
