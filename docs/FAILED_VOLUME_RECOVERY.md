# Failed-volume recovery

A rejected scan stays unavailable to queries; strict NTFS validation is not relaxed.
The volume worker makes one automatic recovery attempt after a two-second delay.
If recovery also fails, the worker waits without rescanning until an explicit
IndexStart request for that failed volume arrives. Ready and scanning volumes
remain idempotent, and each volume retains exactly one worker and snapshot writer.

Retries reuse the existing snapshot/journal recovery path. They do not purge data,
restart the service, or disturb other volumes. Shutdown interrupts the retry wait.
An internal panic requires an explicit retry rather than an automatic repetition.

Deterministic worker tests cover recovery, exhausted retries, explicit retry after
exhaustion, cancellation while waiting, and preserving an unrelated ready volume.
