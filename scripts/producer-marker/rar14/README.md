# RAR 1.3/1.4 producer-marker investigation

Implementation: append a length-delimited `RARS` record to the old `RE~^` main
header, after any archive comment, and increase the main `HeadSize` field.
No flags are changed. There is no generic service-block mechanism in this
family; the first file begins at the end of the main header.

The production writer emits this record in every archive and volume. The
original investigation used `writer-proxy.py` to patch an unmodified writer;
production validation uses the compiled CLI directly. `framing.py` proves that
removing the record and restoring `HeadSize` reproduces each control byte-for-byte,
including its comment, file headers and packed/encrypted payloads.

## Record layout

All integers are little-endian. The marker occupies the end of the main
header, outside the existing length-delimited comment data:

| Bytes | Meaning |
|---|---|
| 4 | `RARS` |
| 2 | Payload byte length |
| variable | `rars/0.10.2\nmarker-schema=1\n` |
| 4 | CRC32 of the payload |

This record adds **38 bytes per archive/volume**. It uses no reserved flag,
does not create a file, does not replace the user's comment, and does not set
or imitate the authenticity-verification flag. The record is public, advisory,
removable and forgeable. The CRC is for recognizing damaged metadata, not
for authentication. The experiment refuses authenticity-bearing inputs.

The comment and padding interpretation is described in the local spec
repository's `doc/RAR13_FORMAT_SPECIFICATION.md`; compatibility is established
by running the reference executables rather than assuming padding is ignored.
The original RAR 1.402 distribution comes from the user's local
`_refs/rarbins/RAR1_402.EXE`, extracted using UnRAR. No reference executables
or third-party source are included here.

## Results

**250 command-line/DOS paired comparisons: 202 pass, 48 baseline failures,
zero marker regressions.** All **10 additional WinRAR 1.54 GUI extraction
pairs pass**, with exact file hashes. Total: **260 paired comparisons across
26 tested reader builds**, including builds that do not support this old format.
All 26 corruption controls detect the damaged member. RAR 1.402 passes all
four update checks and retains the complete marker at the end of the header.

The framing comparison covers 32 marked archive/volume files across the native
and Windows corpora. A final rebuild reproduces all 16 native corpus files
byte-for-byte. Rust validation passes: 1,697 core unit tests, 818 integration
tests, 17 Python and 4 Wasm tests, 2 focused CLI/external-reader tests, and
5 minimal-feature marker tests. Workspace Clippy and formatting checks pass.
Coverage includes framing, rewrites, rejection of damaged/unknown records,
empty archives, comments, passwords, volumes and main-header size limits.
The marked WinRAR 1.54 CRC screenshot was repeated manually after the automatic
capture was blank. A fresh display and 30-second startup wait produced a
readable capture; both variants visibly report the CRC error.

## Coverage

The ten cases are stored, compressed, solid, commented, stored/compressed
passwords, stored/compressed/solid volumes, and an empty file. ASCII names
are used for native/Windows readers and uppercase 8.3 names for DOS/WinRAR
1.54. Rars currently refuses encrypted `rar14` volume creation before output;
that unsupported writer case is excluded rather than counted as a reader pass.

Native readers: UnRAR 7.20, RAR 7.12 and 3.93, 7-Zip 26.04, libarchive 3.8.5,
The Unarchiver 1.10.8, and two retained rars builds. Windows readers: console
RAR 2.90, 3.00, 4.20, 5.00, 6.02, 7.21b1; WinRAR 2.90, 3.00, 4.20, 6.02,
7.21b1; Bandizip 7.47. DOS: RAR 1.402, RAR 1.55, UnRAR 1.05, RAR 2.50 and
UnRAR 2.50. WinRAR 1.54 is exercised through its GUI under explicit `wine32`.

For paired command-line/DOS cases, success requires successful test/list and
extraction plus exact filename/content SHA256 maps, and comment export where
available. WinRAR 1.54 uses the previously established GUI procedure and exact
extraction hashes. Its Test/corruption screenshots require manual review;
GUI process exit status is not an archive validation result.

Baseline limitations observed in both variants:

- 7-Zip, libarchive, The Unarchiver and Bandizip do not successfully read these
  pre-1.5 archives in the tested builds.
- RAR/WinRAR 3.00 fail the solid and solid-volume cases (the ordinary solid
  case can return success while skipping the first member).
- RAR 3.00, 3.93 and 4.20 fail comment export; RAR 5.00 drops the final byte.

These are not marker regressions. The matrix records actual failures rather
than equating a zero exit status with successful extraction.

## Updates and corruption controls

RAR 1.402 updates stored and commented archives in both variants, preserving
all files and comments. Both marked archives retain the marker. This means
it identifies the original rars writer, not necessarily the last updater.
Modern RAR 2.90, 3.93 and 6.02 refuse updating the old format in both variants;
their update failures are retained as baseline results.

Fourteen RAR/WinRAR command-line corruption controls and ten DOS corruption
controls check a bit flip in stored member data. WinRAR 1.54 supplies two
additional GUI controls. The record's own CRC is not checked by these
external readers, which intentionally ignore the extension.

## Reproduction

Retain an unmodified rars CLI as the control (the experiment used `b8b814a`).
Use the compiled production CLI as `--marked`. To reproduce the initial
patching experiment instead, set `RARS_CONTROL` to the control and pass
`writer-proxy.py` as `--marked`. The framing probe uses the fixed tested
version text in `mark.py`. Use the engine JSON format from the
other producer-marker reports.

```sh
python3 scripts/producer-marker/rar14/probe.py --ascii --jobs 4 \
  --control /path/to/control \
  --marked /path/to/production-rars \
  --engines /path/to/native.json --output target/rar14-native

xvfb-run -a python3 scripts/producer-marker/rar14/probe.py --ascii --jobs 6 \
  --control /path/to/control \
  --marked /path/to/production-rars \
  --engines /path/to/windows.json --output target/rar14-windows

python3 scripts/producer-marker/rar14/framing.py target/rar14-native

python3 scripts/producer-marker/rar14/dos.py \
  --control /path/to/control \
  --marked /path/to/production-rars \
  --engines /path/to/dos.json --output target/rar14-dos

xvfb-run -a -s '-screen 0 1280x1024x24' \
  python3 scripts/producer-marker/rar14/winrar154.py \
  --dos target/rar14-dos --exe /path/to/WINRAR.EXE \
  --prefix /path/to/isolated-win32-prefix --output target/rar14-win154
```

DOS engine JSON rows contain `name`, executable `path` and an `unrar` boolean.
The contemporary reader must be named `R140`, which is used for update checks.
The GUI probe requires a fresh `C:\R14GUI` in the prefix (or another short
name supplied with `--work-name`), uses fixed coordinates
without a window manager, and refuses stale output directories. Run Wine jobs
sequentially when sharing a prefix. Raw evidence lives under
`target/producer-marker-rar14/`; `results.json` retains final results and hashes.

## Preserving rewrites and bounds

The shared main-header writer appends the extension, including in every volume,
and rejects headers that cannot fit the record within the 16-bit header size.
Existing comment-length fields are unchanged.

Preserving rewrites accept unmarked legacy inputs and exactly one recognized
record after any comment. Recognition checks the complete record length, a
128-byte payload bound, CRC, schema and version syntax. The writer emits the
current producer version on rewrite. Unknown, duplicated, misplaced or damaged
extensions and unsupported authenticity metadata remain refused.

No claim is made about every historical release, native Windows execution,
original 1.3 executables, SFX stubs, or authenticity-bearing archives.
