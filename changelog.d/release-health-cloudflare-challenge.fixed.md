**The release-health report says when Cloudflare, not colonizer.dev, turned the check away.** A
403 from the site that carries `cf-mitigated: challenge` is Cloudflare's bot protection challenging
the GitHub runner, and the report now says so instead of a bare "HTTP 403", which read like the
page itself was broken.
