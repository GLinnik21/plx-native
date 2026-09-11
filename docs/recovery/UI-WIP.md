# Incomplete Session UI recovery

UI source checkpoint: `77eeec52cd0a99f57a1f02f834ad33eb5c1ec4a8`, based on `9776e84b`.
Safe core recovery tree: `b1af4589d7c1b3a094822e38316eeab75955da60`, capturing `23e4a837`.

Only Login, Profiles and their registry constructor/bound splices are overlaid.
This composed snapshot is NOT integration-ready or release-verified. Login focused
tests passed (21 Login and 47 Profiles). The three queued selection/PIN regressions
now pass with the [selection acknowledgement contract](selection-ack-contract.md).
Full checks still reject legacy dead code and the global CTL owner. Full migration
and full-plan proof remain
incomplete. No private configuration, recordings, images or binary artifacts included.
Raw worker ancestry and working indexes were not published or changed.
