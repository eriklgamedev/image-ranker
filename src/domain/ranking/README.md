# Ranking domain

The public module is `index.ts`. All state is plain JSON data, including the exact
current presentation pair and a retained, chunked decision log. The coordinator
must sort candidates by canonical path with an explicit code-unit comparison before
passing their IDs to `createRun`. Randomization then uses the saved Mulberry32 seed
and Fisher–Yates shuffle. No locale, clock, filesystem, or global randomness is used.

`decide` is immutable and rejects stale pair tokens. Old history chunks and events
are shared; there is no snapshot of the whole run in each history event. Pair
observations occupy paged triangular storage. `undo` replays the retained decisions
except the last, preserving exact presentation sides and scheduler state, and then
mints a new token to reject delayed votes from the old branch. JSON round trips
preserve the current token. The persistence layer should version and retain this
log across checkpoints.

Winner-Stays bands and Adaptive bands are provisional. Adaptive freezes one bounded
refinement schedule and applies cached and newly collected anchor observations to
the same partition operation. Full Pairwise half-point scores order score buckets;
strongly connected components and topological order handle decisive evidence within
a bucket. Its output bands are singleton rows. Draws, cycles, and incomparable
evidence appear as separate annotations, never a transitive draw equivalence.

The configured limit is 10,000 candidates and a conservative maximum 250,000
decisions per run. Winner-Stays is checked against its loose cubic bound because
draws can change the workload; Adaptive is the alternative for larger collections.
Adaptive `baseline` is the binary-insertion comparison upper bound, not a measured
typical estimate; `maximum` adds at most one refinement observation per member.
Winner-Stays `baseline` is the exact no-draw count, and Full Pairwise is exact.

Persistence and UI must independently enforce save acknowledgment, image readiness,
content revisions, keyboard-repeat suppression, and run identity. These pure
reducers cannot establish whether a user-visible image still matches a disk file.
