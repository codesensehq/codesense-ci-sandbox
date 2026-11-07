// SPDX-License-Identifier: MIT
//! Atomic disk evidence for cooperative server replacement.

use codesense_pty_core::protocol::Reply;
use std::path::Path;

pub(crate) fn write(root: &Path, kind: &str, receipt: &Reply) -> Result<(), String> {
    let Reply::Retired { nonce, .. } = receipt else {
        return Err("invalid replacement evidence".into());
    };
    let name = format!("replacement-{kind}-{nonce}.json");
    let final_path = root.join(&name);
    let temp = root.join(format!(".{name}.tmp"));
    let bytes = serde_json::to_vec(receipt).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&temp);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    let written = (|| {
        (&file).write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &final_path).map_err(|e| e.to_string())?;
        std::fs::File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Decode bytes previously read from a replacement evidence file.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn decode(bytes: &[u8]) -> Result<Reply, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_receipt_roundtrips_and_invalid_data_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let receipt = Reply::Retired {
            nonce: "abc".into(),
            server_id: "server".into(),
            epoch: "epoch".into(),
            process_start: 17,
            sessions: 0,
        };
        write(root.path(), "retirement", &receipt).unwrap();
        let bytes = std::fs::read(root.path().join("replacement-retirement-abc.json")).unwrap();
        assert_eq!(decode(&bytes).unwrap(), receipt);
        assert!(decode(b"invalid").is_err());
    }
}
