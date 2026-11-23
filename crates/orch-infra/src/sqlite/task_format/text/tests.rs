use super::*;

#[test]
fn the_threshold_counts_original_bytes_and_tags_cannot_alias_text() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let inline = "é".repeat(INLINE_TEXT_MAX_BYTES / 2);
    assert!(matches!(
        publish(&store, inline.as_bytes()),
        Err(TextError::Inline)
    ));
    let text = format!("{inline}a");
    let (reference, rows) = publish(&store, text.as_bytes()).unwrap();
    assert_eq!(reference.bytes, 65537);
    assert_eq!(reference.chunks, 1);
    assert_eq!(rows[0].size, 65537);
    assert_eq!(reconstruct_text(&store, &reference, &rows).unwrap(), text);
    let tag = Tag { reference };
    let encoded = serde_json::to_string(&tag).unwrap();
    assert_eq!(serde_json::from_str::<Tag>(&encoded).unwrap(), tag);
    let user_string = serde_json::to_string(&encoded).unwrap();
    assert!(serde_json::from_str::<Tag>(&user_string).is_err());
    let (a_reference, _) = publish(&store, &vec![b'a'; 65537]).unwrap();
    assert_eq!(
        a_reference.sha256,
        "008ffc88d3c96a9f307524eb361e47c5222a887fc45fa0c1fb8d429c5c23b430"
    );
}

#[test]
fn reference_tags_refuse_unknown_null_duplicate_and_fractional_fields() {
    let valid = format!(
        r#"{{"$blob":{{"sha256":"{}","bytes":65537,"chunks":1}}}}"#,
        BlobHash::of(b"synthetic")
    );
    for invalid in [
        valid.replace("65537", "65537.0"),
        valid.replace("65537", "6.5537e4"),
        valid.replace("65537", "null"),
        valid.replace("65537", "-1"),
        valid.replace("65537", "18446744073709551616"),
        valid.replace("\"chunks\":1", "\"chunks\":1,\"chunks\":1"),
        valid.replace("\"chunks\":1", "\"chunks\":1,\"extra\":true"),
        valid.replace("\"chunks\":1", "\"extra\":1"),
        valid.replace("}}", "},\"extra\":true}"),
    ] {
        assert!(serde_json::from_str::<Tag>(&invalid).is_err());
    }
}

#[test]
fn exactly_64_mib_is_one_object_and_one_byte_more_splits_inside_utf8() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let mut bytes = vec![b'a'; MAX_BLOB_BYTES as usize];
    let (reference, rows) = publish(&store, &bytes).unwrap();
    assert_eq!(reference.chunks, 1);
    assert_eq!(
        reference.sha256,
        "fae972222d455a2eaee1661ad9625502ec3bfc5ec38b87a6eec5afd5107331b5"
    );
    assert_eq!(reconstruct(&store, &reference, &rows).unwrap(), bytes);
    bytes.push(b'a');
    let (reference, rows) = publish(&store, &bytes).unwrap();
    assert_eq!(
        reference.sha256,
        "0ed59c6929ac1c013be3a95779b6edf64fd7d9858e28fc246964c5bddce58ba2"
    );
    assert_eq!(
        rows.iter().map(|r| r.size).collect::<Vec<_>>(),
        [MAX_BLOB_BYTES, 1]
    );
    assert_eq!(reconstruct(&store, &reference, &rows).unwrap(), bytes);
    bytes.truncate(MAX_BLOB_BYTES as usize - 1);
    bytes.extend_from_slice("é".as_bytes());
    let (reference, rows) = publish(&store, &bytes).unwrap();
    assert_eq!(
        reference.sha256,
        "dfe3c4a85fd6bd7047064e244afd38b700e98bf0f1b2fab53d9905e2467d5120"
    );
    assert_eq!(
        rows[0].sha256,
        "18afcd915bf7c60ef9da9ead13bef131856420e9312ae8aa25d1238d21789d48"
    );
    assert_eq!(
        rows[1].sha256,
        "9e8e8c37a53bac77a653d590b783b2508e8ed2fed040a278bf4f4703bbd5d82d"
    );
    assert_eq!(
        reconstruct_text(&store, &reference, &rows)
            .unwrap()
            .as_bytes(),
        bytes
    );
}

#[test]
fn repeated_chunks_keep_both_ordinals() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let bytes = vec![b'a'; MAX_BLOB_BYTES as usize * 2];
    let (reference, rows) = publish(&store, &bytes).unwrap();
    assert_eq!(
        reference.sha256,
        "3510b7e066e76c8f7c306693c97204824d0c8f92ae6fc8a4c0dd657abf424a1b"
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].sha256, rows[1].sha256);
    assert_eq!((rows[0].ordinal, rows[1].ordinal), (0, 1));
    assert_eq!(reconstruct(&store, &reference, &rows).unwrap(), bytes);
    assert!(reconstruct(&store, &reference, &rows[..1]).is_err());
    let mut reordered = rows.clone();
    reordered.swap(0, 1);
    assert!(matches!(
        reconstruct(&store, &reference, &reordered),
        Err(TextError::Corrupt("chunk ordinal or size"))
    ));
}

#[test]
fn broken_metadata_and_missing_objects_are_typed_without_paths_or_content() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let bytes = vec![b'a'; 65537];
    let (reference, rows) = publish(&store, &bytes).unwrap();
    for broken in [
        Reference {
            sha256: "../private".into(),
            ..reference.clone()
        },
        Reference {
            bytes: 65536,
            ..reference.clone()
        },
        Reference {
            bytes: u64::MAX,
            ..reference.clone()
        },
        Reference {
            chunks: 0,
            ..reference.clone()
        },
        Reference {
            sha256: BlobHash::of(b"other").to_string(),
            ..reference.clone()
        },
    ] {
        assert!(matches!(
            reconstruct(&store, &broken, &rows),
            Err(TextError::Corrupt(_))
        ));
    }
    for broken in [
        Chunk {
            ordinal: 1,
            ..rows[0].clone()
        },
        Chunk {
            size: 65536,
            ..rows[0].clone()
        },
        Chunk {
            sha256: "PRIVATE_SENTINEL".into(),
            ..rows[0].clone()
        },
        Chunk {
            sha256: BlobHash::of(b"missing").to_string(),
            ..rows[0].clone()
        },
    ] {
        let error = reconstruct(&store, &reference, &[broken]).unwrap_err();
        assert!(matches!(error, TextError::Corrupt(_)));
        assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    }
    store.remove(&rows[0].sha256.parse().unwrap()).unwrap();
    assert!(matches!(
        reconstruct(&store, &reference, &rows),
        Err(TextError::Corrupt("chunk object"))
    ));
}

#[test]
fn whole_reconstruction_checks_lengths_and_utf8() {
    let (_temp, data) = crate::sqlite::test_data();
    let store = DiskBlobStore::open(data.root()).unwrap();
    let bytes = vec![0xff; 65537];
    let (reference, rows) = publish(&store, &bytes).unwrap();
    assert_eq!(reconstruct(&store, &reference, &rows).unwrap(), bytes);
    assert!(matches!(
        reconstruct_text(&store, &reference, &rows),
        Err(TextError::Corrupt("UTF-8"))
    ));
    let short = vec![0xff; 65536];
    let short_hash = store.put(&short).unwrap();
    let mut changed = rows;
    changed[0].sha256 = short_hash.to_string();
    assert!(matches!(
        reconstruct(&store, &reference, &changed),
        Err(TextError::Corrupt("chunk object size"))
    ));
}
