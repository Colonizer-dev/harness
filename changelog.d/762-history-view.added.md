**A fleet owner can now read the history its members sync.** Settings → Fleet has a Fleet history
section listing every member's synced colonies, newest first, each marked "finished on <member>",
with filters for member, repository, status and finish date, totals per member and repository
(colonies, merged, cost) at the top, and a drawer with one colony's record and its logs. The same
view is `GET /api/fleet/history` (filtered, cursor-paged, with the totals),
`GET /api/fleet/history/{member}/{row_id}` and `…/logs/{name}`, all owner-only. A removed member's
history stays readable, marked removed under the name it had. Synced rows are kept for
`COLONIZER_FLEET_INGEST_RETENTION_DAYS` (default 90) after they arrive; the five-minute reclaim
tick prunes older ones and the logs only they referenced. ([#762])
