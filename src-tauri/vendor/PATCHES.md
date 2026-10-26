# Patched torrent engine (librqbit 9.0.1)

These three crates are copies of librqbit 9.0.1 from crates.io with FluxDM's changes on top. `Cargo.toml`'s
`[patch.crates-io]` points the build at them. Every change is marked with a `FluxDM patch` comment, so
`grep -rn "FluxDM patch" vendor/` lists them all.

To upgrade librqbit: copy the new versions over these folders, then redo each change below. They're small
and self-contained.

| What | Where | Why |
|---|---|---|
| **Tracker status registry**: state, seeds, leeches, peers received, message, next announce for each tracker | `librqbit-tracker-comms/src/tracker_comms.rs` (`pub mod status`), `tracker_comms_http.rs` (`complete`/`incomplete` are `Option`) | The Trackers tab. librqbit parsed these numbers and threw them away. Reached through `librqbit::tracker_status`. |
| **Peer piece count and interest** (`pieces_have`, `peer_interested` on `PeerStats`) | `librqbit/src/torrent_state/live/peer/stats/snapshot.rs` | Seeds count, per-peer progress and flags. |
| **File priorities**: `ManagedTorrent::set_file_priorities(levels)` reorders the engine's file download queue (higher level first, then name), kept across pause and resume | `librqbit/src/torrent_state/mod.rs`, `torrent_state/live/mod.rs` (`set_file_levels`), `session.rs` | Low / Normal / High / Maximum, like the .NET version. librqbit already had the ordered list internally (by name only). |
| **Finished files held read-only** (`TorrentStorage::on_file_completed`, `OpenedFile::reopen_read_only`), applied when a file's last piece verifies, after the startup check and when a torrent goes live | `librqbit/src/storage/mod.rs`, `storage/filesystem/{fs,opened_file}.rs`, `torrent_state/live/mod.rs`, `torrent_state/initializing.rs` | Windows refuses programs that open a file for reading without sharing write access while anyone holds a write handle. Before this, finished torrent files couldn't be opened by many apps while FluxDM held them. librqbit had a comment saying this should happen. |
| **BEP 55 holepunch**: the `ut_holepunch` message, relay / initiator / target roles | `librqbit-peer-protocol/src/extended/ut_holepunch.rs` (new), `extended/mod.rs`, `lib.rs`; `librqbit/src/torrent_state/live/mod.rs` (`on_holepunch`, `try_holepunch`, `holepunch_connect`), `peer/mod.rs` (`supports_holepunch`), `stream_connect.rs` (`prefer_utp`) | Reaches peers behind NAT/CGNAT through a peer both sides are connected to; the .NET build's MonoTorrent fork had it. When a direct dial to a peer learned over PEX fails, FluxDM asks that PEX source to introduce them. Both sides then dial uTP at the same moment. Counters: `librqbit::holepunch_stats`. |
| **Availability**: `TorrentStateLive::availability()` gives distributed copies of the still-needed pieces among connected peers | `librqbit/src/torrent_state/live/mod.rs` (`needed_pieces`, `availability`) | The Availability column. Below 1.00, some needed piece isn't on any connected peer. |
| **Seed-first peer turnover** (replaces the .NET-fork port): every 5 s, while downloading with 80%+ of slots full and peers waiting, drop up to 8 of the worst connections - leechers with nothing we need, then slow leechers, then idle or slow seeds (under a quarter of our best peer's speed); fast peers of either kind are kept. Per-torrent connection limit default 100 -> 200 (`src/torrent.rs`) | `librqbit/src/torrent_state/live/mod.rs` (`turnover`, `pick_turnover`, `PeerSeen`, `task_peer_turnover`, `turnover_tests`) | On ADSL our upload is small; leechers want it while seeds give their full speed for free. Slots go to the fastest seeds. |
| **PEX shares incoming peers too, with BEP 11 flags** (seed / uTP / holepunch / reachable) | `librqbit-peer-protocol/src/extended/ut_pex.rs` (`from_addrs_with_flags`), `librqbit/src/torrent_state/live/mod.rs` (`pex_candidates`) | librqbit only shared peers it had dialed itself, so peers behind NAT (which only connect in) were never shared. Those are the peers hole-punching is for. |
| **Live speed limits**: `ManagedTorrent::set_ratelimits` changes a running torrent's own limits; `Limit::set` ignores an unchanged value | `librqbit/src/limits.rs`, `torrent_state/mod.rs`, `torrent_state/live/mod.rs` | Changing a limit used to remove and re-add the torrent (a full disk re-check), and re-setting the session limit every second rebuilt its bucket, so the cap leaked a burst each second. |
| **Sequential download**: `ManagedTorrent::set_sequential(bool)`; `ChunkTracker::iter_queued_pieces` walks each file from its first piece to its last when it is on (an `AtomicBool` in `ManagedTorrentOptions`, read on every piece request, so it can be flipped while running) | `librqbit/src/torrent_state/mod.rs`, `torrent_state/live/mod.rs` (`acquire_next_piece`), `piece_tracker.rs` (`set_sequential`), `chunk_tracker.rs` (`iter_queued_pieces`), `session.rs` | "Download in order" in the torrent menu. Stock librqbit always picks first, last, then the middle of each file. |
| **Slow limits and "No upload"**: the limiter bucket holds at least 32 KB (`MIN_BURST`); an upload limit of 1 B/s sets `Limits::upload_blocked`, and `task_upload_scheduler` drops requests while the torrent's or the session's upload is blocked | `librqbit/src/limits.rs` (`is_block`, `upload_blocked`, `fluxdm_tests`), `torrent_state/live/mod.rs` (`task_upload_scheduler`), `src/tests/e2e_no_upload.rs` (new) | Governor refuses to pass more than one bucket at once, and the bucket was one second's worth: under 16 KB/s no 16 KB block could ever pass, the error ended the upload task, and uploads stayed dead even after the limit was raised. The app therefore raised every limit to 32 KB/s, so "No upload" and 20 KB/s both uploaded at 32 KB/s. |

Tested on 2026-10-01:
- **Unit tests:** the engine's own suites pass (librqbit 32, peer-protocol 23 including the new `ut_holepunch` codec test, tracker-comms 4).
- **Live swarm:** tested on the Debian 13.7 netinst swarm. Seeds were counted, per-peer speeds and progress showed, and the tracker reported Working.
- **Hole-punch interop:** relays running other clients answered our rendezvous requests with `connect`.
- **File locking:** files of a finished, loaded torrent open with `FileShare.Read`. They were "in use" before.

Tested on 2026-10-26 (limits patch): the engine's suites pass with the patched siblings (librqbit 36, including the
new `limits::fluxdm_tests` and `tests::e2e_no_upload`). `e2e_no_upload` runs a local seeder with upload blocked:
the downloader gets 0 bytes in 8 s while the seeder's torrent stays Live, then completes once the limit is lifted.
The same test against the unpatched engine fails: the seeder never uploads again after the limit is lifted.
Run them from a copy of `vendor/` with a `[patch.crates-io]` for `librqbit-peer-protocol` and
`librqbit-tracker-comms` pointing at the sibling folders (testing in place would rewrite `librqbit/Cargo.lock`).
