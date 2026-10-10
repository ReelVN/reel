//! Compiler-to-container regression checks; never execute bytecode.
use reel_package::{self as rp, ErrorKind, Limits};

fn artifact(source: &str, entry: Option<&str>) -> reel_cli::Artifact {
    reel_cli::build(&reel_cli::check(source).unwrap(), entry).unwrap()
}
fn u64_at(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    let hex = rp::digest(bytes);
    std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
}
fn repair_index(bytes: &mut [u8]) {
    let offset = u64_at(bytes, 24);
    let hash = hash(&bytes[offset..]);
    bytes[40..72].copy_from_slice(&hash);
}
fn section(bytes: &[u8], index: usize) -> &[u8] {
    let at = u64_at(bytes, 24) + index * rp::INDEX_ENTRY_LEN;
    let start = u64_at(bytes, at + 8);
    &bytes[start..start + u64_at(bytes, at + 16)]
}
/// Independent fixture assembly permits rehashed corrupt metadata to reach the
/// cross-validator rather than stopping at the outer integrity boundary.
fn repack(original: &[u8], json: &[u8]) -> Vec<u8> {
    let b01 = section(original, 0);
    let mut out = vec![0; rp::HEADER_LEN];
    out.extend_from_slice(b01);
    out.extend_from_slice(json);
    let index = out.len();
    for (kind, start, data) in [
        (1u32, rp::HEADER_LEN, b01),
        (2, rp::HEADER_LEN + b01.len(), json),
    ] {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(start as u64).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&hash(data));
    }
    let total = out.len() as u64;
    out[..8].copy_from_slice(rp::MAGIC);
    out[8..10].copy_from_slice(&1u16.to_le_bytes());
    out[16..24].copy_from_slice(&total.to_le_bytes());
    out[24..32].copy_from_slice(&(index as u64).to_le_bytes());
    out[32..40].copy_from_slice(&(2u64 * rp::INDEX_ENTRY_LEN as u64).to_le_bytes());
    repair_index(&mut out);
    out
}

#[test]
fn private_entry_comes_from_declaration_not_initializer_or_linked_global() {
    let a = artifact(
        "fn helper() { 1 } fn main(a = 1, b = a) { let local = 2; let f = || { local + a }; f() + b }",
        Some("main::main"),
    );
    let read = rp::decode(&a.bytes, Limits::default()).unwrap();
    let m = read.metadata();
    let p = read.program().package();
    let entry = m.game_entry.as_ref().unwrap();
    assert_ne!(entry.function_id, m.initializer.function_id);
    assert_eq!(
        p.modules[0].initializer.unwrap().0,
        m.initializer.function_id
    );
    let d = &m.declarations[entry.declaration_index as usize];
    assert_eq!(d.name, "main");
    assert_eq!(
        m.functions[d.source_function_id as usize].output,
        entry.function_id
    );
    assert_eq!(
        p.symbols[p.functions[entry.function_id as usize].name.index()],
        "main"
    );
    assert!(p.modules[0].exports.is_empty());
    assert!(
        p.modules[0].globals[d.global_id as usize]
            .linked_function
            .is_none()
    );
    assert!(p.functions.len() > m.functions.len());
    assert!(m.bindings.iter().enumerate().any(|(i, a)| {
        m.bindings[i + 1..]
            .iter()
            .any(|b| a.source == b.source && a.function != b.function)
    }));
    assert_eq!(a.blake3, rp::digest(&a.bytes));
    assert_eq!(
        rp::encode(section(&a.bytes, 0), m, Limits::default()).unwrap(),
        a.bytes
    );
}

#[test]
fn entry_is_optional_and_only_real_module_functions_can_be_selected() {
    assert!(
        rp::decode(&artifact("let x = 1;", None).bytes, Limits::default())
            .unwrap()
            .metadata()
            .game_entry
            .is_none()
    );
    for (source, entry) in [
        ("let main = || { 1 };", "main::main"),
        ("fn outer() { fn main() { 1 } }", "main::main"),
        ("fn main(x) { x }", "main::main"),
        ("fn main() { 1 }", "other::main"),
        ("fn main() { 1 }", "main::missing"),
    ] {
        let checked = reel_cli::check(source).unwrap();
        let error = reel_cli::build(&checked, Some(entry)).err().unwrap();
        assert_eq!(error.stage, "entry");
        assert!(!error.diagnostics.is_empty());
    }
}

#[test]
fn every_truncation_and_payload_bit_corruption_is_rejected() {
    let a = artifact("fn main() { 1 }", Some("main::main"));
    for end in 0..a.bytes.len() {
        assert!(
            rp::decode(&a.bytes[..end], Limits::default()).is_err(),
            "accepted length {end}"
        );
    }
    for at in [0, 40, 72, u64_at(&a.bytes, 24) - 1] {
        let mut bytes = a.bytes.clone();
        bytes[at] ^= 1;
        assert!(rp::decode(&bytes, Limits::default()).is_err());
    }
    let mut appended = a.bytes;
    appended.push(0);
    assert!(rp::decode(&appended, Limits::default()).is_err());
}

#[test]
fn index_rejects_overflow_overlap_duplicates_versions_and_required_extensions() {
    let original = artifact("fn main() { 1 }", None).bytes;
    let index = u64_at(&original, 24);
    let mutations = [
        (8, vec![2]),
        (10, vec![1]),
        (12, 1u32.to_le_bytes().to_vec()),
        (32, 111u64.to_le_bytes().to_vec()),
        (index + 8, u64::MAX.to_le_bytes().to_vec()),
        (index + 16, u64::MAX.to_le_bytes().to_vec()),
        (index + 8, 0u64.to_le_bytes().to_vec()),
        (index + 8, (index as u64).to_le_bytes().to_vec()),
        (
            index + rp::INDEX_ENTRY_LEN + 8,
            72u64.to_le_bytes().to_vec(),
        ),
        (index + rp::INDEX_ENTRY_LEN, 1u32.to_le_bytes().to_vec()),
        (index, 99u32.to_le_bytes().to_vec()),
        (index + 4, 3u32.to_le_bytes().to_vec()),
    ];
    for (at, value) in mutations {
        let mut bytes = original.clone();
        bytes[at..at + value.len()].copy_from_slice(&value);
        repair_index(&mut bytes);
        assert!(
            rp::decode(&bytes, Limits::default()).is_err(),
            "accepted mutation at {at}"
        );
    }
}

#[test]
fn rehashed_metadata_cannot_redirect_entry_or_lie_about_cross_references() {
    let original = artifact("let x = 1; fn main(a = 2) { a }", Some("main::main")).bytes;
    let initial: serde_json::Value = serde_json::from_slice(section(&original, 1)).unwrap();
    for (pointer, value) in [
        ("/schema_version", serde_json::json!(2)),
        ("/bytecode_blake3", serde_json::json!("wrong")),
        ("/module_id", serde_json::json!(1)),
        ("/canonical_path", serde_json::json!("foreign")),
        ("/source_blake3", serde_json::json!("wrong")),
        ("/source_span/end", serde_json::json!(u32::MAX)),
        ("/initializer/function_id", serde_json::json!(u32::MAX)),
        ("/game_entry/function_id", serde_json::json!(u32::MAX)),
        ("/game_entry/declaration_index", serde_json::json!(u32::MAX)),
        ("/declarations/0/name", serde_json::json!("foreign")),
        ("/declarations/0/global_id", serde_json::json!(u32::MAX)),
        (
            "/declarations/0/source_function_id",
            serde_json::json!(u32::MAX),
        ),
        ("/bindings/0/binding", serde_json::json!(u32::MAX)),
        ("/globals/0/source", serde_json::json!(u32::MAX)),
    ] {
        let mut json = initial.clone();
        *json.pointer_mut(pointer).unwrap() = value;
        assert!(
            rp::decode(
                &repack(&original, &serde_json::to_vec(&json).unwrap()),
                Limits::default()
            )
            .is_err(),
            "accepted {pointer}"
        );
    }
    let mut duplicate = initial.clone();
    let binding = duplicate["bindings"][0].clone();
    duplicate["bindings"].as_array_mut().unwrap().push(binding);
    assert!(
        rp::decode(
            &repack(&original, &serde_json::to_vec(&duplicate).unwrap()),
            Limits::default()
        )
        .is_err()
    );
}

#[test]
fn json_is_bounded_before_materialization_and_duplicate_fields_are_rejected() {
    let original = artifact("fn main() { 1 }", None).bytes;
    let json = std::str::from_utf8(section(&original, 1)).unwrap();
    let duplicate = format!("{{\"schema_version\":1,{}", &json[1..]);
    assert_eq!(
        rp::decode(&repack(&original, duplicate.as_bytes()), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::Metadata
    );
    let duplicate_nested = json.replace("\"initializer\":{", "\"initializer\":{\"module_id\":0,");
    assert_eq!(
        rp::decode(
            &repack(&original, duplicate_nested.as_bytes()),
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::Metadata
    );
    let mut missing_entry: serde_json::Value = serde_json::from_str(json).unwrap();
    missing_entry.as_object_mut().unwrap().remove("game_entry");
    assert!(
        rp::decode(
            &repack(&original, &serde_json::to_vec(&missing_entry).unwrap()),
            Limits::default()
        )
        .is_err()
    );
    for bad in [
        format!("{}0{}", "[".repeat(17), "]".repeat(17)),
        format!("\"{}\"", "x".repeat(4097)),
        "{\"schema_version\":18446744073709551616}".into(),
    ] {
        assert!(rp::decode(&repack(&original, bad.as_bytes()), Limits::default()).is_err());
    }
    let limits = Limits::default();
    for restricted in [
        Limits {
            max_file_bytes: original.len() - 1,
            ..limits
        },
        Limits {
            max_sections: 1,
            ..limits
        },
        Limits {
            max_metadata_bytes: json.len() - 1,
            ..limits
        },
        Limits {
            max_metadata_records: 1,
            ..limits
        },
        Limits {
            max_json_depth: 1,
            ..limits
        },
        Limits {
            max_json_string_bytes: 1,
            ..limits
        },
        Limits {
            max_json_tokens: 1,
            ..limits
        },
        Limits {
            max_work: 1,
            ..limits
        },
    ] {
        assert_eq!(
            rp::decode(&original, restricted).unwrap_err().kind,
            ErrorKind::Limit
        );
    }
}

#[test]
fn optional_sections_are_ignored_but_verified_and_required_unknowns_fail() {
    let original = artifact("fn main() { 1 }", None).bytes;
    let index = u64_at(&original, 24);
    let payload = b"future optional data";
    let mut extended = original[..index].to_vec();
    extended.extend_from_slice(payload);
    let new_index = extended.len();
    extended.extend_from_slice(&original[index..]);
    extended.extend_from_slice(&99u32.to_le_bytes());
    extended.extend_from_slice(&0u32.to_le_bytes());
    extended.extend_from_slice(&(index as u64).to_le_bytes());
    extended.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    extended.extend_from_slice(&hash(payload));
    let total = extended.len() as u64;
    extended[16..24].copy_from_slice(&total.to_le_bytes());
    extended[24..32].copy_from_slice(&(new_index as u64).to_le_bytes());
    extended[32..40].copy_from_slice(&(3u64 * rp::INDEX_ENTRY_LEN as u64).to_le_bytes());
    repair_index(&mut extended);
    rp::decode(&extended, Limits::default()).unwrap();
    let mut corrupted = extended.clone();
    corrupted[index] ^= 1;
    assert_eq!(
        rp::decode(&corrupted, Limits::default()).unwrap_err().kind,
        ErrorKind::Integrity
    );
    extended[new_index + 2 * rp::INDEX_ENTRY_LEN + 4] = 1;
    repair_index(&mut extended);
    assert_eq!(
        rp::decode(&extended, Limits::default()).unwrap_err().kind,
        ErrorKind::Version
    );
}

#[test]
fn valid_container_hashes_do_not_bypass_b01_validation() {
    let mut bytes = artifact("fn main() { 1 }", None).bytes;
    bytes[rp::HEADER_LEN] ^= 1; // damage inner B01 magic
    let index = u64_at(&bytes, 24);
    let digest = hash(section(&bytes, 0));
    bytes[index + 24..index + 56].copy_from_slice(&digest);
    repair_index(&mut bytes);
    assert_eq!(
        rp::decode(&bytes, Limits::default()).unwrap_err().kind,
        ErrorKind::Bytecode
    );
}

#[test]
fn unicode_span_boundaries_and_all_mapping_keys_are_checked() {
    let original = artifact("fn 中文(a = 1) { a }", Some("main::中文")).bytes;
    let initial: serde_json::Value = serde_json::from_slice(section(&original, 1)).unwrap();
    let mut broken_span = initial.clone();
    broken_span["declarations"][0]["declaration_span"]["start"] = serde_json::json!(4);
    assert!(
        rp::decode(
            &repack(&original, &serde_json::to_vec(&broken_span).unwrap()),
            Limits::default()
        )
        .is_err()
    );
    for array in ["functions", "globals", "declarations"] {
        let mut duplicate = initial.clone();
        let value = duplicate[array][0].clone();
        duplicate[array].as_array_mut().unwrap().push(value);
        assert!(
            rp::decode(
                &repack(&original, &serde_json::to_vec(&duplicate).unwrap()),
                Limits::default()
            )
            .is_err(),
            "accepted duplicate {array}"
        );
    }
}

#[test]
fn ordinary_thousand_function_module_fits_default_static_budgets() {
    let mut source = String::new();
    for index in 0..1000 {
        source.push_str(&format!("fn f{index}() {{ {index} }}\n"));
    }
    let a = artifact(&source, Some("main::f999"));
    let read = rp::decode(&a.bytes, Limits::default()).unwrap();
    assert_eq!(read.metadata().declarations.len(), 1000);
    assert!(a.bytes.len() < Limits::default().max_file_bytes);
}

#[test]
fn repeated_source_slots_must_keep_their_b01_binding_contract() {
    let original = artifact("fn first(a) { a } fn second(b) { b }", None).bytes;
    let mut json: serde_json::Value = serde_json::from_slice(section(&original, 1)).unwrap();
    assert_eq!(json["bindings"].as_array().unwrap().len(), 2);
    json["bindings"][1]["source"] = json["bindings"][0]["source"].clone();
    let error = rp::decode(
        &repack(&original, &serde_json::to_vec(&json).unwrap()),
        Limits::default(),
    )
    .unwrap_err();
    assert!(error.detail.contains("binding identity changes"));
}
