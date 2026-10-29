# RAR 2.0 producer marker

The `rar20` writer emits the existing `RARS` new-subblock after any old-style
archive comment and before files, in every volume. Its unpacker-version field
is **20**, matching this target; RAR 2.9/3/4 markers retain version 29. All other
fields and the payload schema are unchanged: type `0x7a`, flags `0xc000`,
36-byte header, stored method `0x30`, zero host/attributes/time, and
`rars/<version>\nmarker-schema=1\n`. At version 0.10.2 it adds 64 bytes per volume.

Strict recognition accepts only unpacker versions 20 and 29, alongside the
existing exact framing, bounded payload, CRC and schema checks. Preserving
rewrites infer the minimum version from both members and the producer, so a
RAR 2.0 archive remains RAR 2.0. A newer producer record can still impose a
higher minimum. RAR 1.5 now uses unpacker version 15; see the
[RAR 1.5 report](../rar15/README.md). RAR 1.4 output remains unmarked.

## Compatibility evidence

**242 paired comparisons across 22 executable builds: 232 pass, 10 baseline
failures, zero marker regressions.** All 18 corruption controls and six archive
update checks pass.

`results.json` records the control writer, marked writer, reader fingerprints,
paired outcomes, corruption controls and update checks. The control writer is
from the preceding RAR 2.9 implementation (`8d17a44`); its RAR 2.0 output is
unmarked. The integration changes to recognition/version inference do not alter
the prototype's emitted marker bytes. The native matrix is repeated using the
integration writer and reader.

Native and Windows matrices contain **220 pairs across 20 builds: 210 pass,
10 baseline failures, zero marker regressions**. Both RAR and WinRAR 2.90 pass
all 11 cases. Cases cover stored/compressed/solid data, archive comments,
stored/compressed password protection, stored/compressed/solid/password volumes,
and an empty file. Native filenames include Unicode; Windows uses ASCII, as in
the preceding investigations. A passing pair requires test/list success and
exact extracted filename/content-SHA256 maps. RAR comment export is checked
separately; UnRAR's displayed comment is used because it has no `cw` command.

Baseline failures remain visible: libarchive fails nine control shapes
(compressed, solid, comments, passwords and volumes); RAR 5.00's comment export
loses the final character in both variants. The Unarchiver passes all 11 cases. These are not passing compatibility cases. All 14 native RAR
and WinRAR corruption controls reject flipped member data with exit code 3.
Raw framing checks validate 38 archive/volume files, including header and
payload CRCs. Four RAR 2.90/3.93 update checks preserve member contents and the
comment while dropping the producer as its flags request.

`dos.py` tests the original **DOS RAR 2.50 and UnRAR 2.50** executables extracted
from the local reference distribution, under DOSBox-X. Both pass all 11 cases
(22 pairs), four corruption controls return exit 3, and both RAR 2.50 update
checks preserve files/comments while dropping the marker. It uses equivalent
8.3 uppercase filenames, not lossy DOS aliases for the Windows corpus. It
checks test/extract exit status, exact filenames/content hashes, comment export,
corruption rejection, and updates by RAR 2.50. The distribution's `TECHNOTE.TXT`
documents the common skip/discard (`0x4000`) and additional-data-size (`0x8000`)
flags; the declared block list stops before `0x7a`. Actual reader behavior is
therefore tested rather than inferred from recognizing the subblock type.

This is evidence for the tested executable builds, not every historical 2.x
release or native Windows environment. The DOS reference available locally is
2.50, not 2.00. No production RAR 2.x recovery generation is added. The marker
is advisory, removable and forgeable, and contains no user filenames or data.

## Reproduce

Build `rars-cli` at `8d17a44` as the control, then build this change as the marked
writer. Supply engine JSON configurations as in the
[parent investigation](../README.md). Reader paths and hashes are retained in
the results. Keep the Wine invocations sequential when sharing a prefix.

```sh
python3 scripts/producer-marker/rar20/probe.py --jobs 4 \
  --control /path/to/control --marked /path/to/marked \
  --engines /path/to/native.json --output target/rar20-native

xvfb-run -a python3 scripts/producer-marker/rar20/probe.py --ascii --jobs 6 \
  --control /path/to/control --marked /path/to/marked \
  --engines /path/to/windows.json --output target/rar20-windows

python3 scripts/producer-marker/rar20/dos.py \
  --control /path/to/control --marked /path/to/marked \
  --tools /path/to/extracted-rar250 --output target/rar20-dos

xvfb-run -a python3 scripts/producer-marker/rar20/maintenance.py \
  --matrix target/rar20-windows --engines /path/to/rar-update-engines.json \
  --output target/rar20-maintenance

xvfb-run -a python3 scripts/producer-marker/rar20/negative.py --jobs 5 \
  --matrix target/rar20-windows --engines /path/to/rar-winrar-engines.json \
  --output target/rar20-negative
```

The scripts do not modify the source or install software. `--resume` supports
interrupted native/Windows matrices with unchanged writers and engine configs.
DOSBox uses a bounded run with maximum CPU cycles; the initial default-speed
run hit its time limit and is not counted as compatibility evidence. All local
artifacts and command logs are under `target/producer-marker-rar20/`.

## Validation

The final workspace library run passes: 1,696 core tests, 17 Python tests and
four Wasm tests. Focused producer/rewrite tests, legacy integration targets,
CLI integration/publication/verification tests, the required RAR 2.0 external
decoder test, minimal-write marker tests, Clippy with warnings denied, and
formatting checks pass. The member-metadata mutation fixture explicitly removes
the producer so its format inference is determined by the mutated member.
