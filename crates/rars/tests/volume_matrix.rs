//! Cross-family volume-set contract tests.

use rars::{volume_members, Archive, ArchiveFamily, ArchiveReader, Error};

fn archive_set(parts: &[&[u8]]) -> Vec<Archive> {
    parts
        .iter()
        .map(|part| ArchiveReader::read_owned((*part).to_vec()).unwrap())
        .collect()
}

fn assert_ordered_set(parts: &[&[u8]], family: ArchiveFamily, encrypted: bool) {
    let archives = archive_set(parts);
    assert!(archives.iter().all(|archive| archive.family() == family));
    let members = volume_members(&archives).expect("ordered volume set");
    assert!(!members.is_empty());
    assert!(members.iter().all(|member| member.meta.family == family));
    assert!(members
        .iter()
        .all(|member| member.meta.is_encrypted == encrypted));
    assert!(members.iter().all(|member| !member.meta.is_split_before));
    let mut reversed = archives;
    reversed.reverse();
    assert!(
        volume_members(&reversed).is_err(),
        "reversed set was accepted"
    );
}

#[test]
fn volume_members_covers_each_stable_rar_family_and_split_encryption() {
    assert_ordered_set(
        &[
            include_bytes!("fixtures/rar13/MULTIVOL.RAR"),
            include_bytes!("fixtures/rar13/MULTIVOL.R00"),
            include_bytes!("fixtures/rar13/MULTIVOL.R01"),
            include_bytes!("fixtures/rar13/MULTIVOL.R02"),
        ],
        ArchiveFamily::Rar13,
        false,
    );
    assert_ordered_set(
        &[
            include_bytes!("fixtures/rar15_40/rar300/stored_multivol_rar300.rar"),
            include_bytes!("fixtures/rar15_40/rar300/stored_multivol_rar300.r00"),
            include_bytes!("fixtures/rar15_40/rar300/stored_multivol_rar300.r01"),
            include_bytes!("fixtures/rar15_40/rar300/stored_multivol_rar300.r02"),
        ],
        ArchiveFamily::Rar15To40,
        false,
    );
    assert_ordered_set(
        &[
            include_bytes!("fixtures/rar50/encrypted_multivol.part1.rar"),
            include_bytes!("fixtures/rar50/encrypted_multivol.part2.rar"),
            include_bytes!("fixtures/rar50/encrypted_multivol.part3.rar"),
        ],
        ArchiveFamily::Rar50Plus,
        true,
    );
}

#[test]
fn volume_members_rejects_mixed_families_before_folding_split_members() {
    let legacy =
        ArchiveReader::read_owned(include_bytes!("fixtures/rar13/MULTIVOL.RAR").to_vec()).unwrap();
    let modern =
        ArchiveReader::read_owned(include_bytes!("fixtures/rar50/multivol.part1.rar").to_vec())
            .unwrap();
    assert!(matches!(
        volume_members(&[legacy, modern]),
        Err(Error::InvalidHeader(_))
    ));
}
