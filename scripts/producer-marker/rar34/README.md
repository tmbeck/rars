# RAR 3/4 producer-marker investigation

The `rar30` and `rar40` writers now emit the named `RARS` new-subblock described
below. Preserving rewrites validate and replace recognized records; unknown,
duplicated, misplaced or damaged records are refused. The subsequent
[RAR 2.9 investigation](../rar29/README.md) established that
the marker itself only requires RAR 2.9; rewrites now infer that minimum unless
comments or encrypted headers require RAR 3. RAR 5/7 retain their existing wire
representation and now share the producer payload definition. RAR 2.9 is now
also marked; the subsequent [RAR 2.0 backfit](../rar20/README.md) uses unpacker
version 20 for its producer. RAR 1.5 now uses unpacker version 15; see the
[RAR 1.5 report](../rar15/README.md). RAR 1.4 output remains unmarked.

`prototype.patch` records the original investigation against revision `a5e7723`.
`results.json` preserves that historical evidence. Production writer comparisons
and the additional recovery checks are recorded separately in
`production-results.json`. Do not apply the prototype over the current source.

The integration writer was rerun through the complete matrix: **540 comparisons,
500 passing pairs, 40 baseline failures, zero marker regressions**. This repeats
the original 17 executable builds and adds the current production reader (30
passing comparisons). The baseline failures are unchanged. All 36 additional
recovery-protected comparisons pass. Production archive corruption controls also
require exit code 3 from native RAR and all four WinRAR GUI builds.

## Candidate record

Place a `NEWSUB_HEAD` (`0x7a`) immediately after the main header, before any
user comment or file, in every volume. Use the file-like new-subblock framing:

| Field | Value |
| --- | --- |
| Name | `RARS` |
| Common flags | `0xc000`: data present, discard unknown block on update |
| Header length | 36 bytes |
| Unpacker version | 29 |
| Method | `0x30` (stored) |
| Host OS, attributes, timestamp | Zero |
| Extra fields, salt, solid/dependency flags | None |
| Header integrity | Low 16 bits of the normal CRC32 |
| Data integrity | Normal file-data CRC32 |

The payload is the same advisory text used by the RAR 5/7 implementation:

```text
rars/0.10.2
marker-schema=1
```

The version identifies the producing library, not necessarily an embedding
application. At this version length the record adds 64 bytes. Header encryption
encrypts and pads the header, adding 84 bytes instead; the payload remains public
and searchable without the password. No user filenames or member data are
exposed by this record. It does not appear as an extracted file or replace a
user comment. It is removable and forgeable, not authentication.

RARLAB's [RAR 4 technical note, preserved in the Debian package source](https://sources.debian.org/src/rar/2%3A4.0.b3-1/technote.txt/)
documents block type `0x7a` and the common `0x4000`/`0x8000` flags. The existing
writer's new-subblock comment implementation supplies the file-like framing.
`RARS` is a chosen name, not a namespace allocation from RARLAB.

## Experiment

`probe.py` creates marked and unmarked pairs with otherwise identical writer
sources. Both formats exercise 15 shapes: stored, compressed, solid, user
comment, stored and compressed password protection, stored and compressed
encrypted headers, solid + encrypted headers + comment, stored volumes,
compressed volumes, solid volumes, password volumes, encrypted-header volumes,
and an empty file. Legacy volume output currently supports one member; those
cases split the binary member across three volumes.

A passing pair requires both test/list and extraction commands to succeed,
and exact extracted filename/content-SHA256 maps to match the inputs. Merely
returning exit zero is insufficient. The corpus contains binary data, repeated
text, an empty file, and UTF-8 text. The native matrix also uses a Unicode
filename; the Windows matrix uses an ASCII spelling of that name.

The separate initial Windows Unicode probe failed controls: readers including
RAR/WinRAR 3.00 and 4.20 interpreted the existing writer's raw UTF-8 name
differently. Both variants tested and extracted, but their filename manifests
were wrong. Those are baseline failures, not passing marker cases. The ASCII
matrix isolates the marker question without claiming to fix filename encoding.

Native baseline limitations also remain: official 7-Zip 26.04 rejects both
encrypted-header volume controls; libarchive 3.8.5 fails the solid, encrypted,
or multivolume controls; The Unarchiver 1.10.8 fails the encrypted controls.
The reports retain these as `CONTROL_FAIL`, never as passing compatibility.

The completed main matrices contain **510 case pairs: 470 pass, 40 fail their
controls, and zero marker regressions**. Each executable has 30 comparisons
(15 shapes in each format). The partial Unicode Windows diagnostic is recorded
separately and is not included in these totals.

| Executable | Passed pairs | Control failures | Marker regressions |
| --- | ---: | ---: | ---: |
| UnRAR 7.20, Linux | 30 | 0 | 0 |
| RAR 7.12, Linux | 30 | 0 | 0 |
| RAR 3.93, Linux | 30 | 0 | 0 |
| Official 7-Zip 26.04, Linux | 28 | 2 | 0 |
| libarchive 3.8.5, Linux | 8 | 22 | 0 |
| The Unarchiver 1.10.8, Linux | 16 | 14 | 0 |
| Unmodified rars 0.10.2 reader, Linux | 30 | 0 | 0 |
| RAR 3.00 console, Wine | 30 | 0 | 0 |
| WinRAR 3.00 GUI executable, Wine/Xvfb | 30 | 0 | 0 |
| RAR 4.20 console, Wine | 30 | 0 | 0 |
| WinRAR 4.20 GUI executable, Wine/Xvfb | 30 | 0 | 0 |
| RAR 5.00 console, Wine | 30 | 0 | 0 |
| RAR 6.02 console, Wine | 30 | 0 | 0 |
| RAR 7.21 beta 1 console, Wine | 30 | 0 | 0 |
| WinRAR 6.02 GUI executable, Wine/Xvfb | 30 | 0 | 0 |
| WinRAR 7.21 beta 1 GUI executable, Wine/Xvfb | 30 | 0 | 0 |
| Bandizip 7.47 (binary banner says Beta), Wine | 28 | 2 | 0 |

Bandizip's two failures are the encrypted-header volume controls and their
marked counterparts. `results.json` contains case definitions, outcomes,
producer binary hashes and reader executable fingerprints. Successful content
maps are represented by their equality to the case's expected manifest;
failure manifests are retained explicitly. Full command logs remain locally.

All 20 negative controls pass: native RAR 3.93 and the WinRAR 3.00, 4.20,
6.02 and 7.21 beta 1 GUI executables each return CRC-error exit code 3 for
corrupted control and marked archives in both formats. Silent GUI testing
therefore did not mask member-data corruption.

`maintenance.py` independently walks all 100 generated native archive/volume
files, validating plaintext header CRCs and marker framing/payload CRCs.
Encrypted headers are exercised by the reader matrix; their producer payload
presence is checked without a password.

RAR 3.93 maintenance checks, for both formats and both variants:

- Adding a member preserves the user comment and all member contents, and
  drops the unknown producer record.
- Adding recovery data (`rr10`) also drops the producer record. A subsequent
  deliberate member-data corruption returns CRC error 3, repair succeeds, and
  every extracted content hash matches the original.

Adding recovery with RAR does not itself test repair with the marker present,
so a follow-up closes that gap. `protected_marker.py` first reproduces RAR 3.93's
sector tags and XOR parity byte-for-byte. It then constructs a recovery record
covering the marked prefix and retains RAR's ENDARC framing. Deliberately damaged
member data is repaired correctly for both formats and both variants; the
marked archives retain the marker and all original content hashes are restored.
An additional run corrupts the producer payload as well as member data; repair
restores the entire protected prefix byte-for-byte, including the producer.
The protected fixtures pass all 36 reader comparisons (18 executable builds,
including the production reader, in both formats). This generator is an
investigation tool; production RAR 3/4 recovery generation remains unsupported.

## Reproducing

Build the CLI at the source revision above and copy it to `rars-control`.
Apply `prototype.patch`, build again, and copy that binary to `rars-marked`.
Restore the source afterward. Keep binaries and Wine prefixes on disk; `/tmp`
is RAM-backed on the development machine.
For the production comparison, use the unchanged baseline binary as the control
and build the current CLI as the marked writer instead of applying the prototype.

The harness reuses the parent directory's engine adapters. Supply an engine
JSON list using the same `name`, `family`, `command`, `windows`, and `env`
fields described in the [RAR 5 investigation](../README.md). Native and Wine
executable paths and fingerprints are retained in the result evidence.

```sh
python3 scripts/producer-marker/rar34/probe.py \
  --control /path/to/rars-control --marked /path/to/rars-marked \
  --engines /path/to/native-engines.json --output target/rar34-native

xvfb-run -a python3 scripts/producer-marker/rar34/probe.py --ascii --jobs 5 \
  --control /path/to/rars-control --marked /path/to/rars-marked \
  --engines /path/to/wine-engines.json --output target/rar34-windows

python3 scripts/producer-marker/rar34/maintenance.py \
  --matrix target/rar34-native --rar /path/to/rar393 \
  --output target/rar34-maintenance

xvfb-run -a python3 scripts/producer-marker/rar34/negative.py --jobs 5 \
  --matrix target/rar34-windows --engines /path/to/rar-and-winrar-engines.json \
  --output target/rar34-negative

python3 scripts/producer-marker/rar34/protected_marker.py \
  --matrix target/rar34-native --maintenance target/rar34-maintenance \
  --rar /path/to/rar393 --output target/rar34-protected

python3 scripts/producer-marker/rar34/protected_readers.py --jobs 4 \
  --archives target/rar34-protected --input target/rar34-native/input \
  --engines /path/to/native-engines.json --output target/rar34-protected-readers
```

Use fresh output directories, or `--resume` for an interrupted matrix with
unchanged binaries, engines and case definitions. Readers run independently
with separate extraction directories. Negative controls flip a stored member
byte and require CRC-error exit code 3 from RAR/WinRAR, including the GUI
executables' silent command-line mode.

The WinRAR 3.00/4.20 application directories were copied from the user's
reference prefixes; those prefixes were not modified. Wine used the existing
isolated `target/producer-marker/wine` prefix and Xvfb. Local archives and full
command logs are under `target/producer-marker-rar34/`.

## Production integration

The ordinary and both split-volume writer paths emit the marker through their
existing output paths, with cancellation checks around emission. Recognition
requires the exact framing, bounded payload size, valid data CRC and known
schema; unknown or damaged records cannot silently disappear during preserving
rewrites. The current producer version replaces a recognized old version.
Archive comment metadata is selected by `CMT`, not by subblock position.

Regression coverage includes marker placement, low-level zero-member output,
empty files in the external corpus, comments, encrypted headers, every volume,
old producer replacement, ambiguous metadata refusal and earlier formats staying
unmarked. Existing restrictions on empty legacy preserving rewrites remain.

Reader compatibility is evidence for this candidate, not proof about every
archiver. Wine tests do not establish native Windows behavior or manual GUI
interaction. No RAR 1.x/2.x backfit is implied by these results.
