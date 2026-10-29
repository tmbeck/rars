# RAR 1.5 producer marker

The `rar15` writer emits the existing `RARS` new-subblock after any old-style
archive comment and before files, in every volume. Preserving rewrites recognize
valid version-15 producer records, replace them with the current producer
version, and retain the RAR 1.5 minimum requirement. Unknown, damaged, duplicate
or misplaced producer records are refused by preserving rewrites.

The record uses type `0x7a`, flags `0xc000`, a 36-byte header, unpacker version
**15**, stored method `0x30`, and zero host/attributes/time. Its payload is the
existing `rars/<version>\nmarker-schema=1\n` text. It follows any old-style
archive comment and precedes files, in every volume; at rars 0.10.2 it adds
64 bytes. The marker remains advisory, removable and forgeable.

`prototype.patch` retains the original writer-only experiment against `3ce92d9`.
Production also recognizes unpacker version 15 for safe producer replacement.

## Results

**242 paired comparisons across 22 working reader builds: 227 pass, 15 baseline
failures, zero marker regressions.** A further **11 GUI extraction pairs pass
in WinRAR 1.54**, bringing coverage to 23 reader builds. The results below were
rerun against the production implementation. Both DOS RAR 1.55 and its bundled UnRAR 1.05
pass all 11 cases: stored, compressed, solid, archive comment, stored/compressed
passwords, stored/compressed/solid/password volumes, and an empty file. All
18 corruption controls return CRC-error exit code 3.

A passing pair requires successful test/list and extraction commands and exact
filename/content-SHA256 maps. Native tests include a Unicode filename; modern
Windows readers use its ASCII spelling. DOS uses uppercase 8.3 names to avoid
conflating marker behavior with filename limitations. Archive comment export
is checked separately, with displayed text used for native UnRAR.

The baseline read/extract failures are:

- libarchive: compressed, solid, comments, passwords and volumes (nine cases).
- 7-Zip: three password cases.
- The Unarchiver: solid mode.
- RAR 5.00: comment export drops the final character.
- Bandizip: stored password-protected data.

These fail their unmarked controls too and are not passing compatibility cases.
The other working native/Windows engines are the same builds used in the
[RAR 2.0 report](../rar20/README.md). `results.json` records their hashes,
case definitions and individual outcomes.

### Contemporary readers

The local **WinRAR 1.54 beta** is a 16-bit Windows executable. Initial
command-line failures were a test-environment problem: Wine 10.0's experimental
WoW64 mode could not initialize `krnl386.exe16`, even without an archive or
with the original `UNRARS10.RAR` from the UnRAR 1.05 distribution.

Installing `wine32:i386` and using `/usr/bin/wine32` explicitly with a fresh
`WINEARCH=win32` prefix fixes startup. On this Ubuntu installation `/usr/bin/wine`
still chooses `wine64`, so installing the package alone is insufficient.
WinRAR 1.54 accepts an archive filename to open, but the modern `t`/`x` command
syntax used by the other GUI probes leaves this version in its file browser.
The original archive was extracted through the GUI; all six files match the
bundled original source byte-for-byte. Subsequent probes use the GUI's Test
and To cur dir buttons with all archive members selected, DOS 8.3 paths, and
SHA256 checks of extracted files. The multi-volume dialog must be accepted;
password cases need the password entered and Yes to all for subsequent files.

**All 11 GUI extraction pairs pass** with exact filename/SHA256 maps: stored,
compressed, solid, commented, stored/compressed passwords, all four volume
cases, and empty files. Both deliberately corrupted stored variants display
`CRC failed in BINARY.BIN` in the Test screenshots. These GUI results are recorded
separately from the 242 command-line/DOS comparisons above; archive-comment
text was not separately exported by this GUI probe. This verifies the marker
with one additional reader build. It does not cover WinRAR 1.54 rewriting.
The production rerun uses `winrar154.py`; its screenshots and JSON evidence
are under `target/producer-marker-rar15/production-win154/`. `results.json`
retains all 22 extraction manifests and screenshot hashes. Earlier exploratory
GUI results and runtime diagnostics are retained separately as historical
evidence. The production probe explicitly accepts the all-volumes prompt.

**RAR 1.55 updating an archive with a comment fails for both variants.** Adding
a member returns success, but subsequent comment export reports a broken
comment and extraction rejects the updated archive. This is a baseline
rars/RAR 1.55 interoperability issue, not a successful update test. Both updated
archives lack the marker. All four RAR 2.90/3.93 update checks pass, retaining files/comments and
discarding the marker. Independent framing checks cover 38 archive/volume files. No claim is made about RAR 1.50/1.51 executables or every historical
release, native Windows, or recovery protection.

## References and reproduction

The [archived RAR 1.55 shareware distribution](https://ftp.zx.net.nz/pub/archive/ftp.icm.edu.pl/pub/coast/msdos/archiver/rar155.exe)
is 269,885 bytes. Its original `TECHNOTE.DOC` documents the common `0x4000`
skip/discard and `0x8000` additional-data flags, though its block-type list does
not include `0x7a`. The bundled `UNRAR105.EXE` is a self-extracting distribution;
extract it to obtain UnRAR 1.05. Its bundled source's `ReadBlock` seeks past
unknown records using header size plus the optional packed-data length.
Distribution, executable, document and source hashes are retained in the report.
No reference binaries or third-party source are committed here.

Build `rars-cli` at `3ce92d9` and retain it as the unmarked control. Build the
current implementation as the marked binary. Supply engine JSON files as in
the preceding reports. Raw archives/logs are local to
`target/producer-marker-rar15/`; production reruns use the `production-*` paths.

```sh
python3 scripts/producer-marker/rar15/probe.py --jobs 4 \
  --control /path/to/control --marked /path/to/marked \
  --engines /path/to/native.json --output target/rar15-native

xvfb-run -a python3 scripts/producer-marker/rar15/probe.py --ascii --jobs 6 \
  --control /path/to/control --marked /path/to/marked \
  --engines /path/to/windows.json --output target/rar15-windows

python3 scripts/producer-marker/rar15/dos.py \
  --control /path/to/control --marked /path/to/marked \
  --tools /path/to/rar155-with-unrar105 --output target/rar15-dos

xvfb-run -a python3 scripts/producer-marker/rar15/negative.py --jobs 5 \
  --matrix target/rar15-windows --engines /path/to/rar-winrar-engines.json \
  --output target/rar15-negative

xvfb-run -a python3 scripts/producer-marker/rar15/maintenance.py \
  --matrix target/rar15-windows --engines /path/to/rar-update-engines.json \
  --output target/rar15-maintenance
```

Run Wine jobs sequentially when sharing a prefix. UnRAR 1.05 uses `-o+`, not
`-y`; the initial invocation with the newer switch printed usage and was
replaced before collecting its compatibility evidence. The DOS verifier records
update failures separately; `--verify-existing` re-evaluates completed command
logs without rerunning DOS. Python scripts compile successfully. Production validation includes the workspace
library suite, core integration tests, RAR 1.5 CLI/external-reader checks, minimal
write-only producer tests, formatting and Clippy.

### Reproducing the WinRAR 1.54 GUI checks

Install `wine32:i386`, initialize a fresh prefix with `WINEARCH=win32` and
`WINELOADER=/usr/bin/wine32 /usr/bin/wine32 wineboot -u`, and stop that prefix's
Wine server before changing Xvfb displays. Use the DOS corpus generated above.
The GUI probe requires a fresh `C:\R15GUI` directory and refuses existing output:

```sh
xvfb-run -a -s '-screen 0 1280x1024x24' \
  python3 scripts/producer-marker/rar15/winrar154.py \
  --dos target/rar15-dos --exe /path/to/WINRAR.EXE \
  --prefix /path/to/isolated-win32-prefix --output target/rar15-win154
```

This uses XTest to select members and invoke Test and To cur dir. It checks
all extracted filenames and hashes automatically. Review its screenshots for
comments and CRC errors: GUI process exit codes do not prove archive validity.
The fixed coordinates assume the original 1.54 beta window without a window
manager. Screenshots and diagnostic binaries remain local artifacts; no
third-party executable or source is committed.
