# RAR 5 producer-marker investigation

The CLI defaults to `rar50`. Its writer now emits the producer service described
below, as does the shared RAR 7 writer. RAR 3/4 use a corresponding
[new-subblock marker](rar34/README.md); earlier formats remain unmarked. Preserving
rewrites replace an intact schema-1 record with the current producer version;
unknown, duplicate, misplaced or damaged records remain errors.

The interoperability investigation used source revision
`f9060ef3dada3c16c313968e283ba37cb0a0ee66` (0.10.2) plus `prototype.patch`.
`results.json` is that historical evidence; it is not a claim to have tested
future library or decoder versions.

## Record format

Use an archive-level RAR 5 service block immediately after the main header,
before any archive comment or file. Its name is `RARS`; its stored payload is:

```text
rars/0.10.2
marker-schema=1
```

The name identifies the producer; the version is the **library** version, not
necessarily the embedding application's version. The schema line versions this
marker representation separately from rars itself.

The block uses type 3, common flags `0x0006` (data present, skip unknown blocks
on update), file flags `0x0004` (CRC32), stored compression information 0, zero
attributes and host OS 0. It has no extra records, timestamps, file dependency
flag, or quick-open cache entry. Both header and payload have their normal CRCs.
The writer's existing layout and recovery calculations include the new block.
Each volume receives its own mark. At this version string length, overhead is
50 bytes per plain-header archive/volume, or 76 with encrypted headers.

The service header follows the archive's header-encryption
policy, but its payload remains plaintext. This intentionally allows the
producer/version string to be found without a password; it does not expose file
names or contents. Structured recognition of the service header
requires the password when headers are encrypted.

The [official RAR 5 format description](https://www.rarlab.com/technote.htm)
defines service blocks and extension framing. `RARS` is our chosen name, not
an officially allocated service name. The experiment establishes reader behavior;
it does not establish a private namespace reservation by RARLAB.

## Observed compatibility

`results.json` records the completed matrix and tool fingerprints. Every case
has a control archive from the unmodified writer and a marked archive from the
prototype. Success requires test/list command success, extraction success, and
an exact map of extracted filenames to SHA-256 content hashes. This also catches
a reader mistakenly extracting the marker as an extra file or silently omitting
members. Inputs include binary data, repeated text, an empty file, and a Unicode
filename. This is a small deterministic corpus, not exhaustive codec testing.

The 14 cases are stored, default compression, solid, archive comment, quick-open,
recovery record, password-protected data, encrypted headers, combined solid +
encrypted headers + recovery + comment, ordinary volumes, solid volumes,
encrypted-header volumes, recovery volumes, and a lone empty file.

| Tested executable | Passed case pairs | Control failures | Marker regressions |
| --- | ---: | ---: | ---: |
| UnRAR 7.20, Linux | 14 | 0 | 0 |
| RAR 7.12, Linux | 14 | 0 | 0 |
| Official 7-Zip 26.04 (`7zz`), Linux | 14 | 0 | 0 |
| libarchive 3.8.5 (`bsdtar`), Linux | 7 | 7 | 0 |
| The Unarchiver 1.10.8 (`unar`/`lsar`), Linux | 13 | 1 | 0 |
| RAR 5.00, Windows command line under Wine | 14 | 0 | 0 |
| RAR 6.02, Windows command line under Wine | 14 | 0 | 0 |
| RAR 7.21 beta 1, Windows command line under Wine | 14 | 0 | 0 |
| WinRAR 6.02 (`WinRAR.exe`), under Wine/Xvfb | 14 | 0 | 0 |
| WinRAR 7.21 beta 1 (`WinRAR.exe`), under Wine/Xvfb | 14 | 0 | 0 |
| Bandizip 7.47 (binary banner says Beta), Windows console under Wine | 10 | 4 | 0 |
| Unmodified rars 0.10.2 reader, Linux | 14 | 0 | 0 |

The libarchive invocation fails the encrypted or multivolume controls. The
Unarchiver invocation fails the encrypted-header multivolume control. In some
cases a tool exits successfully but produces incomplete output, which the hash
manifest catches. These are **not passing interoperability cases** and are not
attributed to the marker. No marked case regressed a passing control.
Bandizip rejects all four multivolume controls with `Cannot open archive
(0xa0000044)`, and rejects their marked counterparts identically. Its remaining
ten cases pass. This investigation does not diagnose those baseline failures.

The distribution's 7-Zip 26.00 package lacks required RAR codecs, so it was
replaced in the matrix by the official binary. Merely finding a `7z` command is
not sufficient evidence of RAR support.

The WinRAR follow-up uses the actual GUI executables copied from
`~/src/tmp/rars-bench/_refs/wineprefixes/winrar602` and `winrar721` into the
isolated test directory. The source prefixes are not modified. Both applications
are invoked with `t` and `x` under Wine/Xvfb, using `-ibck -inul -y` and the test
password. This exercises WinRAR itself, separately from the `Rar.exe` console
tests. It does not exercise manual menu navigation. Four negative controls
(both builds, marked and unmarked archives) flip a byte of stored file data;
every test returns CRC-error exit code 3, verifying that silent mode does not
turn integrity failures into successful test results.

Additional checks with native RAR 7.12:

- Update a commented archive by adding a member: extraction remains correct,
  the user comment is preserved, and the unknown producer service is removed.
  The mark therefore describes original production, not permanent authorship.
- Flip a byte inside a file's packed data in a recovery-bearing archive:
  testing reports corruption; recovery repair succeeds; all extracted hashes
  match the originals; the repaired marked archive retains its producer block.
- Independently walk all 44 generated archive/volume headers, checking header
  CRCs and producer record framing where headers are plaintext. Encrypted
  headers are checked through the external readers instead.

Earlier exploratory probes found that a private **main-header extra-record
type** passed UnRAR but failed libarchive. The named service is preferable on
the evidence here. Those exploratory probes are not part of this matrix.

## Limits and release decision

This is strong evidence for the record format, not proof about every archiver or
every version. WinZip, Windows Explorer, macOS GUI applications, mobile readers,
SharpCompress, and Go rardecode have not been exercised. Windows executables
were tested under Wine, not on native Windows. WinRAR's GUI executable was
automated through its command arguments; manual GUI interactions remain untested. PeaZip
[documents its use of 7-Zip/UnRAR backends](https://peazip.github.io/peazip_help.pdf),
but testing those engines is not a separate PeaZip application test.

Do not make a universal compatibility claim on this evidence alone. Missing
target applications can be added to the matrix. Production regression tests
cover marker placement, empty archives, encrypted headers, every volume, and
preserving rewrites, alongside the existing resource-limit/cancellation tests.
The historical external matrix covers RAR 5; the shared RAR 7 path is also
covered by Rust regression tests and the repository's external decoder tests.

Identification is advisory: the mark can be removed or forged and does not
authenticate an archive. Existing archives remain unmarked. Reserved fields,
authenticity records, and changes to the RAR signature are unnecessary.

## Reproducing

`prototype.patch` preserves the exact investigation-only writer change against
the source revision above. Production code now incorporates the marker plus
rewrite handling and regression coverage; do not apply the historical patch
over the current tree. No PR or tracker comment was created.

Build separate binaries from the same revision in a disposable working copy:
build the unmodified CLI and copy it to `rars-control`; apply `prototype.patch`,
build again and copy it to `rars-marked`. Restore the patch afterward. Keep build
trees and Wine prefixes on disk, not RAM-backed `/tmp`.

The harness itself never patches sources or installs software. Supply an engine
JSON array with `name`, `family` and a `command` argument array. Supported families
are `rar`, `winrar`, `7zip`, `libarchive`, `unar`, `rars`, and `bandizip`. `unar` also needs a
`list_command` for `lsar`. Windows commands use `windows: true`, a `wine` command
prefix and an isolated `WINEPREFIX` in the optional `env` object. Example:

```json
[
  {"name": "unrar", "family": "rar", "command": ["/usr/bin/unrar"]},
  {"name": "7zip", "family": "7zip", "command": ["/path/to/7zz"]}
]
```

```sh
python3 scripts/producer-marker/probe.py \
  --control /path/to/rars-control --marked /path/to/rars-marked \
  --engines /path/to/engines.json --output target/producer-marker/run

python3 scripts/producer-marker/maintenance.py \
  --matrix target/producer-marker/run --rar /path/to/rar \
  --output target/producer-marker/maintenance
```

For `WinRAR.exe`, use family `winrar` and run the harness under `xvfb-run -a`.
The additional CRC-error control is reproducible with:

```sh
xvfb-run -a python3 scripts/producer-marker/negative.py \
  --matrix target/producer-marker/run --engines /path/to/winrar-engines.json \
  --output target/producer-marker/winrar-negative
```

Use a fresh output directory. `--resume` retains completed case/engine pairs;
`--reuse-archives` permits testing existing pairs with another engine matrix.
Command logs, generated archives, extracted files and full JSON reports remain
under the requested output directory. A `CONTROL_FAIL` is recorded explicitly;
only a `REGRESSION` makes the matrix harness exit unsuccessfully. Always inspect
the report for coverage rather than interpreting exit 0 as universal success.

Local evidence for this run is under `target/producer-marker/{native-final,
windows,bandizip,maintenance-final,winrar-gui,winrar-negative-final}`. Tools came from existing RAR distributions,
Ubuntu packages extracted locally, the
[official 7-Zip download](https://www.7-zip.org/download.html), and the
[official Bandizip download](https://www.bandisoft.com/bandizip/).
