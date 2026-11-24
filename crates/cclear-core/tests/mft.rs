use cclear_core::win::mft::{DataRun, parse_record, parse_runs};

#[test]
fn runs_decode_signed_offsets_and_reject_sparse_or_unterminated_data() {
    assert_eq!(
        parse_runs(&[0x11, 3, 42, 0x11, 2, 0xfc, 0]).unwrap(),
        [
            DataRun {
                lcn: 42,
                clusters: 3
            },
            DataRun {
                lcn: 38,
                clusters: 2
            }
        ]
    );
    for invalid in [
        &[0x11, 3, 42][..],
        &[0x01, 3, 0],
        &[0x11, 3, 0xff, 0],
        &[0x11, 0, 8, 0],
    ] {
        assert!(parse_runs(invalid).is_err());
    }
}

#[test]
fn record_checks_sector_fixup_and_reads_file_name_and_data() {
    let mut record = [0u8; 1024];
    record[..4].copy_from_slice(b"FILE");
    put(&mut record, 4, 48, 2);
    put(&mut record, 6, 3, 2);
    put(&mut record, 16, 7, 2);
    put(&mut record, 20, 56, 2);
    put(&mut record, 22, 1, 2);
    put(&mut record, 24, 192, 4);
    put(&mut record, 48, 0xbeef, 2);
    put(&mut record, 50, 0x1234, 2);
    put(&mut record, 52, 0x5678, 2);
    put(&mut record, 510, 0xbeef, 2);
    put(&mut record, 1022, 0xbeef, 2);
    let name_attr = 56;
    put(&mut record, name_attr, 0x30, 4);
    put(&mut record, name_attr + 4, 96, 4);
    put(&mut record, name_attr + 16, 70, 4);
    put(&mut record, name_attr + 20, 24, 2);
    let value = name_attr + 24;
    let parent = 5 | (3 << 48);
    put(&mut record, value, parent, 8);
    record[value + 64] = 2;
    record[value + 65] = 3;
    put(&mut record, value + 66, u64::from('样'), 2);
    put(&mut record, value + 68, u64::from('本'), 2);
    let data_attr = 152;
    put(&mut record, data_attr, 0x80, 4);
    put(&mut record, data_attr + 4, 32, 4);
    put(&mut record, data_attr + 16, 5, 4);
    put(&mut record, data_attr + 20, 24, 2);
    put(&mut record, 184, u64::from(u32::MAX), 4);
    let mut damaged = record;
    damaged[510] ^= 1;
    assert!(parse_record(&mut damaged, 512, false).is_err());
    let parsed = parse_record(&mut record, 512, false).unwrap().unwrap();
    assert_eq!(parsed.sequence, 7);
    assert_eq!(parsed.names, [(parent, "样本".into())]);
    assert_eq!((parsed.logical, parsed.allocated), (5, 0));
    assert_eq!(&record[510..512], &[0x34, 0x12]);
}

fn put(bytes: &mut [u8], offset: usize, value: u64, width: usize) {
    bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

#[test]
fn record_zero_reads_nonresident_data_runs() {
    let mut record = [0u8; 1024];
    record[..4].copy_from_slice(b"FILE");
    put(&mut record, 4, 48, 2);
    put(&mut record, 6, 3, 2);
    put(&mut record, 20, 56, 2);
    put(&mut record, 22, 1, 2);
    put(&mut record, 24, 136, 4);
    put(&mut record, 48, 77, 2);
    put(&mut record, 510, 77, 2);
    put(&mut record, 1022, 77, 2);
    let data = 56;
    put(&mut record, data, 0x80, 4);
    put(&mut record, data + 4, 72, 4);
    record[data + 8] = 1;
    put(&mut record, data + 32, 64, 2);
    put(&mut record, data + 40, 16_384, 8);
    put(&mut record, data + 48, 16_000, 8);
    record[data + 64..data + 68].copy_from_slice(&[0x11, 4, 52, 0]);
    put(&mut record, 128, u64::from(u32::MAX), 4);
    let parsed = parse_record(&mut record, 512, true).unwrap().unwrap();
    assert_eq!(
        parsed.runs,
        [DataRun {
            lcn: 52,
            clusters: 4
        }]
    );
    assert_eq!((parsed.logical, parsed.allocated), (16_000, 16_384));
}
