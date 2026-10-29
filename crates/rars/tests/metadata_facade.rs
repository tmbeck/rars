use rars::{ArchiveMemberDetail, ArchiveReader, ArchiveVersion, Builder};

#[test]
fn rar5_member_facade_publishes_expected_integrity_metadata() {
    let mut builder = Builder::new(ArchiveVersion::Rar50).store(true);
    builder
        .add_bytes(b"file".to_vec(), b"rar5 payload".to_vec(), None, None)
        .unwrap();
    let archive = ArchiveReader::read_owned(builder.to_bytes().unwrap()).unwrap();
    let member = archive.members().next().unwrap();

    assert_eq!(member.meta.expected_unpacked_size, Some(12));
    match member.detail {
        ArchiveMemberDetail::Rar50Plus {
            crc32,
            expected_crc32,
            expected_unpacked_size,
            expected_blake2sp,
            version,
            owner,
            ..
        } => {
            assert!(crc32.is_some());
            assert_eq!(expected_crc32, crc32);
            assert_eq!(expected_unpacked_size, Some(12));
            assert!(expected_blake2sp.is_some());
            assert_eq!(version, None);
            assert_eq!(owner, None);
        }
        _ => panic!("expected RAR5 detail"),
    }
}

#[test]
fn rar4_member_facade_publishes_expected_integrity_and_legacy_times() {
    let mut builder = Builder::new(ArchiveVersion::Rar40).store(true);
    builder
        .add_bytes(b"file".to_vec(), b"rar4 payload".to_vec(), None, None)
        .unwrap();
    let archive = ArchiveReader::read_owned(builder.to_bytes().unwrap()).unwrap();
    let member = archive.members().next().unwrap();

    assert_eq!(member.meta.expected_unpacked_size, Some(12));
    match member.detail {
        ArchiveMemberDetail::Rar15To40 {
            crc32,
            expected_crc32,
            ctime,
            atime,
            owner,
            ..
        } => {
            assert_eq!(expected_crc32, crc32);
            assert_eq!(ctime, None);
            assert_eq!(atime, None);
            assert_eq!(owner, None);
        }
        _ => panic!("expected RAR4 detail"),
    }
}

#[test]
fn split_members_keep_logical_expected_size_and_crc() {
    let payload = b"split member metadata".repeat(20);
    let mut builder = Builder::new(ArchiveVersion::Rar40)
        .store(true)
        .volume_size(Some(80));
    builder
        .add_bytes(b"split".to_vec(), payload.clone(), None, None)
        .unwrap();
    let volumes = builder.build_volumes(None).unwrap();
    assert!(volumes.len() > 1);

    let members: Vec<_> = volumes
        .iter()
        .map(|volume| {
            ArchiveReader::read_owned(volume.clone())
                .unwrap()
                .members()
                .next()
                .unwrap()
        })
        .collect();
    assert!(members.iter().any(|member| member.meta.is_split_before));
    assert!(members.iter().any(|member| member.meta.is_split_after));
    for member in members {
        assert_eq!(
            member.meta.expected_unpacked_size,
            Some(payload.len() as u64)
        );
        let ArchiveMemberDetail::Rar15To40 { expected_crc32, .. } = member.detail else {
            panic!("expected RAR4 detail");
        };
        assert!(expected_crc32 != 0);
    }
}
