# Volume capability matrix

This matrix describes the supported volume-reader surface and the regression
tests that keep it honest. “Split member” means a logical member whose packed
data continues across physical parts; callers pass parts in archive order.
`volume_members` folds continuation headers into one logical member, while
`extract_volumes_to` streams the same set to the caller.

| RAR family | Volume forms and cases covered | Focused tests |
| --- | --- | --- |
| RAR 1.3/1.4 | Old `.RAR`/`.R00` naming; stored and compressed split members, solid continuation, directories, comments, Unicode/password fixtures | `rar13_fixtures.rs`, `legacy_name_encoding.rs`, `volume_matrix.rs` |
| RAR 1.5–4.0 | Old naming and padded `.partNN.rar`; stored/compressed and solid members, per-file/header encryption, comments, extended timestamps, links/directories | `rar15_40_fixtures.rs`, `builder_timestamps.rs`, `volume_matrix.rs` |
| RAR 5/7 | `.partN.rar` and padded naming; stored/compressed and solid members, encrypted data/headers, comments, Unicode names, directories/links, recovery services | `rar50_split_limits.rs`, `rar50_fixtures.rs`, `rar50_golden.rs`, `builder_symlinks.rs`, `volume_matrix.rs` |

## Ordering and deliberate refusals

The Rust volume APIs do not auto-sort parts. A reversed set, a set starting
with a continuation, an empty set, or mixed archive families is rejected
instead of silently producing truncated or reordered output. These
cross-family assertions live in `crates/rars/tests/volume_matrix.rs`.

The writer deliberately refuses features that cannot be represented safely in
the requested volume format: RAR5 file comments in volume output
(`builder_file_comments.rs`), links in legacy volume output
(`builder_symlinks.rs`), unsupported legacy volume options and filters
(`legacy_writer_errors.rs`), preservation rewrites of existing volume layouts
(`rewrite_settings.rs`), and the legacy managed-memory policy before member
I/O (`writer_preparation_limit.rs`). These refusals avoid silently dropping
comments, link targets, timestamps, encryption state, or volume boundaries.

## Remaining gaps

There is not yet a checked-in, reference-generated RAR7 split archive covering
every combination of header encryption, comments, links, and recovery records.
RAR7 is currently covered through the RAR5/7 reader family and writer-generated
RAR70 cases. Filesystem reassembly for directories and links remains
adapter-specific; core tests assert metadata and link targets rather than
platform-specific extraction behavior.
