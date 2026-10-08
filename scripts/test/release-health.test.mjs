// The colonizer.dev half of release-health.mjs: a page that could not be read must not be reported
// as a page whose content is wrong. These tests drive the site checks with a stubbed fetch, so they
// are entirely offline: no network, no GitHub, and no real clock (the retry backoff is injected).
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { fetchSiteText, siteChecks } from '../release-health.mjs';

const TAG = 'v0.2.14';
const HOME = 'https://colonizer.dev/';
const CHANGELOG = 'https://colonizer.dev/docs/changelog';
/** The site as it is meant to look: an install line on the home page, the tag on the changelog. */
const PAGES = {
  [HOME]: `<code id="install-cmd"><span class="install__l">https://colonizer.dev/install.sh</span></code>`,
  [CHANGELOG]: `# Changelog\n\n## ${TAG} - 2026-10-08\n`,
};
const okText = (name) => (name === 'website'
  ? '**website** — colonizer.dev shows the install line'
  : `**changelog** — colonizer.dev/docs/changelog names ${TAG}`);

/** A fetch stub returning `responses` in turn, recording how many times it was called. */
function stub(responses) {
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url);
    const next = responses[Math.min(calls.length - 1, responses.length - 1)];
    if (next instanceof Error) throw next;
    return { ok: next.status === 200, status: next.status, text: async () => next.body };
  };
  return { calls, fetchImpl, fetchSite: (url) => fetchSiteText(url, { fetchImpl, delay: async () => {} }) };
}

/** The site checks over pages answered by `pages`, which maps a URL to its body or its status. */
function check(pages) {
  const fetchSite = async (url) => {
    const page = pages[url];
    if (page === undefined) throw new Error(`unexpected fetch of ${url}`);
    if (typeof page === 'number') throw new Error(`HTTP ${page}`);
    return page;
  };
  return siteChecks(TAG, { fetchSite });
}

const find = (results, name) => results.find((r) => r.text.startsWith(`**${name}**`));

test('both site checks pass when the install line is on the home page and the changelog names the tag', async () => {
  const results = await check(PAGES);
  assert.deepEqual(results.map((r) => r.text), [okText('website'), okText('changelog')]);
  assert.ok(results.every((r) => r.ok));
});

test('a changelog page that does not name the tag still fails with the content message, unchanged', async () => {
  // The website repo lags behind, and that is a real failure the check must keep asserting.
  const [website, changelog] = await check({ ...PAGES, [CHANGELOG]: '# Changelog\n\n## v0.2.8 - 2026-09-30\n' });
  assert.equal(website.ok, true);
  assert.equal(changelog.ok, false);
  assert.equal(changelog.text, `**changelog** — colonizer.dev/docs/changelog does not name ${TAG}`);
});

test('a home page with the install line removed fails with the content message, unchanged', async () => {
  const [website] = await check({ ...PAGES, [HOME]: '<p>Colonizer</p>' });
  assert.equal(website.ok, false);
  assert.equal(website.text, '**website** — colonizer.dev has no https://colonizer.dev/install.sh install line');
});

test('a 403 that outlives the retries is reported as a fetch failure, not as missing content', async () => {
  const [website] = await check({ [HOME]: 403, [CHANGELOG]: PAGES[CHANGELOG] });
  assert.equal(website.ok, false);
  assert.doesNotMatch(website.text, /has no|does not name/);
  assert.match(website.text, /could not read https:\/\/colonizer\.dev\/ \(HTTP 403\)/);
});

test('a transient 403 is retried and the check passes when the next fetch succeeds', async () => {
  const { calls, fetchSite } = stub([{ status: 403 }, { status: 200, body: PAGES[HOME] }]);
  const [website] = await siteChecks(TAG, { fetchSite });
  assert.equal(calls.filter((u) => u === HOME).length, 2, 'the 403 was retried');
  assert.equal(website.ok, true);
  assert.equal(website.text, okText('website'));
});

test('a 404 is the site’s answer and is not retried', async () => {
  const { calls, fetchImpl } = stub([{ status: 404 }]);
  await assert.rejects(fetchSiteText(HOME, { fetchImpl, delay: async () => {} }), /HTTP 404/);
  assert.equal(calls.length, 1);
});

test('a thrown network error is retried, and reports the fetch failure when every attempt fails', async () => {
  const flaky = stub([new TypeError('fetch failed'), { status: 200, body: PAGES[HOME] }]);
  assert.equal(await fetchSiteText(HOME, { fetchImpl: flaky.fetchImpl, delay: async () => {} }), PAGES[HOME]);
  assert.equal(flaky.calls.length, 2, 'the dropped connection was retried');

  const dead = stub([new TypeError('fetch failed')]);
  const [website] = await siteChecks(TAG, { fetchSite: dead.fetchSite });
  assert.equal(website.ok, false);
  assert.match(website.text, /could not read https:\/\/colonizer\.dev\/ \(fetch failed\)/);
  assert.doesNotMatch(website.text, /has no/);
});

test('a 500 is retried too, and the backoff is the injected delay', async () => {
  const waits = [];
  const { calls, fetchImpl } = stub([{ status: 503 }, { status: 500 }, { status: 200, body: PAGES[HOME] }]);
  const body = await fetchSiteText(HOME, { fetchImpl, delay: async (ms) => waits.push(ms) });
  assert.equal(body, PAGES[HOME]);
  assert.equal(calls.length, 3);
  assert.ok(waits.length === 2 && waits[0] < waits[1], `the backoff grows: ${waits}`);
});