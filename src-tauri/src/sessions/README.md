# Session journal ownership

Event and checkpoint payloads remain schema 2. Physical journal ownership uses a
separate version 1 `session-journal.authority.json` envelope. The checksum and
embedded native file identities bind the authority object, identity metadata
object, journal object, parent directory, logical session/log lineage and the
generation's initial complete event prefix. A journal is named
`session-events-<physical-generation-UUID>.log`.

The store retains the qualified file and parent handles through validation,
append, history reads and torn-tail repair. A failed write or sync invalidates
the in-memory revision/index. Before another acknowledgement or checkpoint,
the same handle must be replayed and synced; partial suffix bytes are preserved
and synced before that same handle is truncated. A complete uncertain event
retains its original event ID and is acknowledged once on retry.

## Compatibility and authority

| Input | Allowed action | Preserved evidence |
| --- | --- | --- |
| Owned generation with matching native identities and lineage | Replay, append, checkpoint and same-handle final-tail repair | All complete events; copied partial suffix |
| Schema 2 `identity.json` and path-only `events.log` | Stable read; validate complete prefix; copy into a newly created owned generation; publish authority before new ACK | Entire original `events.log`, including any torn suffix |
| Legacy storage without a valid logical identity | Restricted recovery | All original files |
| New-generation evidence without authority, including interrupted pre-publication migration | Restricted recovery; no automatic fallback or orphan cleanup | Legacy log, candidate generation and any authority temporary |
| Corrupt, future or substituted authority/journal/identity, or replaced parent | Restricted recovery; no append/truncation | Current and retained files |

Migration never adopts a legacy object as a truncation/deletion target. The
observed legacy file identity and hash are audit evidence only. Interrupted
migration after durable authority publication can reopen normally; interruption
before publication deliberately requires recovery review, preserving both
generations instead of guessing which candidate can be deleted or resumed.

The current qualified handle adapter is Windows-only. Other platforms reject
native mutation. Tests here cover actual Windows replacement/sharing attempts
and deterministic injected write/sync/ACK failures; they do not claim power-loss
durability or cross-platform qualification. Snapshot replacement remains a
separate managed-state replacement path; this module's ownership lease applies
to the event journal and its immutable authority.
