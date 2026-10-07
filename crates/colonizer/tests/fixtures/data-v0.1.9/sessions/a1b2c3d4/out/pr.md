# Session storage backend abstraction

Adds `SessionStore` with the local directory layout as the default backend and an
object-store reference backend, and migrates `persist_sessions` onto it.

Closes #325.
