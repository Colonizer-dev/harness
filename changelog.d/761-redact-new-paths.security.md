**Every newer write path redacts secrets with the one shared redactor.** The session store now
redacts each line appended through it, so a new writer to a colony's event, log or findings ledger
cannot forget to. The fleet history push redacts each log before hashing and uploading it, so a
log written before redaction existed no longer reaches the fleet owner with a secret in it. A log
archive redacts every text file it carries (`pr.md`, `review.md`, staged vault notes), not only
logs. The operator vault and the deja-vu transcript copies run the shared redactor after their
exact-value scrub, catching credentials nobody saved, and text sent to Jev goes through it too.
([#761])
