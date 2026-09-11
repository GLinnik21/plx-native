# Incomplete Session UI recovery

UI source checkpoint: `424d7d824dc80df9f809119a2ddbf469073745e2`, based on `9776e84b`.
Safe core recovery parent: `13abf13b33e442445316dcc1741437a03bc5fb26`, capturing `c4f10510`.

Only Login, Profiles and their registry constructor/bound splices are overlaid.
This composed snapshot is NOT integration-ready or release-verified. Login focused
tests passed; Profiles has three observed runtime failures in queued selection/PIN
handling. The approved [selection acknowledgement contract](selection-ack-contract.md)
is not implemented here yet. Full physical Session migration and full-plan proof remain
incomplete. No private configuration, recordings, images or binary artifacts included.
Raw worker ancestry and working indexes were not published or changed.
