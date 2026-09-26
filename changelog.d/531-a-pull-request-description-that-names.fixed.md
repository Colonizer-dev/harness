- **A pull request description that names a file it did not create no longer holds autopilot.**
  The done-claim verifier treated every path in `pr.md` as a claimed change, so a docs-only colony
  that explained its file name by pointing at a `docs/remote-access.md` it had deliberately avoided
  came back `contradicted` — the same path listed twice — and autopilot held. A described path that
  is missing now only contradicts the claim when **none** of the description's in-repo paths is on the
  branch or in the diff (and no changed file is named in it): the work it describes is not there.
  Otherwise the missing path is an advisory, recorded in the verification's new `advisories`, shown
  once in the activity line and added to the published pull request as a verification note, without
  changing the verdict. Empty branches (unverifiable) and failing tests (contradicted) are unchanged.
  ([#531])

[#531]: https://github.com/Colonizer-dev/harness/issues/531
