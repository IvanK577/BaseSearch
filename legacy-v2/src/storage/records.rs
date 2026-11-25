use xxhash_rust::xxh3::Xxh3;

pub fn canonical_record_hash(values: &[String], extra: Option<&str>) -> [u8; 16] {
    let mut hasher = Xxh3::new();
    for value in values {
        let len = value.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(value.as_bytes());
    }
    if let Some(extra) = extra {
        hasher.update(&(extra.len() as u64).to_le_bytes());
        hasher.update(extra.as_bytes());
    }
    hasher.digest128().to_le_bytes()
}
