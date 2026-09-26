- **History no longer repeats or re-dates events.** The page dated each colony by its `updated_at`,
  which moves on every housekeeping write — a reclaim sweep marking worktrees cleaned up, an update or
  restart touching every colony — so one sweep re-dated days-old outcomes to "just now" and drew them as
  a burst of identical lines, and three colonies launched on the same issue read as one event repeated.
  Outcomes now come from the activity log at the time they happened; a colony that finished before the
  log existed is shown once, with its time marked approximate, and every row names its colony. ([#527])

[#527]: https://github.com/Colonizer-dev/harness/pull/527
