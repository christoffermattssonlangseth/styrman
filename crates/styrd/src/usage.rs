use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use styr_core::phone::UsageProvider;

const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
struct Snapshot {
    version: u32,
    providers: Vec<UsageProvider>,
}

pub(crate) fn read() -> Vec<UsageProvider> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let path = PathBuf::from(home).join("Library/Application Support/Karv/Usage/snapshot-v1.json");
    if !path.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Vec::new();
    }
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_SNAPSHOT_BYTES
    {
        return Vec::new();
    }
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Vec<UsageProvider> {
    let Ok(Snapshot {
        version: 1,
        providers,
    }) = serde_json::from_slice(bytes)
    else {
        return Vec::new();
    };
    providers
        .into_iter()
        .filter(|provider| {
            matches!(provider.id.as_str(), "claude" | "codex" | "gemini")
                && !provider.windows.is_empty()
                && provider
                    .windows
                    .iter()
                    .all(|window| (0.0..=1.0).contains(&window.used_fraction))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_karv_v1_and_ignores_unknown_versions() {
        let fixture = r#"{"version":1,"providers":[{"id":"codex","estimated":false,
            "observed_at_ms":1800000000000,"windows":[{"title":"5h Window",
            "used_fraction":0.6,"resets_at_ms":1800000060000,"rolling":false}]}]}"#;
        let providers = decode(fixture.as_bytes());
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].windows[0].used_fraction, 0.6);
        assert!(decode(fixture.replacen("1", "2", 1).as_bytes()).is_empty());
        assert!(decode(fixture.replace("0.6", "1.6").as_bytes()).is_empty());
    }
}
