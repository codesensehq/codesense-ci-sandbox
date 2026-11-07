// SPDX-License-Identifier: MIT
//! The signed updater test fixtures and the bad signatures every update path must refuse.

/// The throwaway test key. The real updater keys are never used by tests.
pub(crate) const TEST_PUBKEY: &str =
    include_str!("../../tests/fixtures/updater/test-updater.key.pub");

/// The updater public key a channel config ships, read from that config's JSON.
pub(crate) fn shipped_pubkey(conf: &str) -> String {
    serde_json::from_str::<serde_json::Value>(conf)
        .unwrap()
        .pointer("/plugins/updater/pubkey")
        .and_then(|k| k.as_str())
        .map(str::to_string)
        .expect("the channel's updater key")
}

/// A bad signature for the signed 0.0.32 fixture, as a manifest carries it, and the public key
/// the app verifies it with.
pub(crate) struct BadSignature {
    pub(crate) case: &'static str,
    /// `None`: the manifest has no `signature` field.
    pub(crate) sig: Option<String>,
    pub(crate) pubkey: String,
}

pub(crate) const ANOTHER_KEY: &str = "signed with another key";

/// The bad signatures a build must refuse, all for the signed 0.0.32 fixture: the fixture's own
/// signature broken each way and checked with the test key, and the fixture's signature intact
/// but checked with `other_key`.
pub(crate) fn bad_signatures(other_key: &str) -> Vec<BadSignature> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let good = include_str!("../../tests/fixtures/updater/update-0.0.32.app.tar.gz.sig").trim();
    let text = String::from_utf8(STANDARD.decode(good).unwrap()).unwrap();
    // A minisign signature file: untrusted comment, signature (algorithm, key id, then the
    // 64-byte signature), trusted comment, global signature.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "the fixture's signature file");
    let mut sig = STANDARD.decode(lines[1]).unwrap();
    *sig.last_mut().unwrap() ^= 0xff;
    let corrupted = format!(
        "{}\n{}\n{}\n{}\n",
        lines[0],
        STANDARD.encode(sig),
        lines[2],
        lines[3]
    );
    let test_key = TEST_PUBKEY.trim();
    let row = |case: &'static str, sig: Option<String>, pubkey: &str| BadSignature {
        case,
        sig,
        pubkey: pubkey.to_string(),
    };
    vec![
        row("corrupted", Some(STANDARD.encode(corrupted)), test_key),
        row(
            "cut in the trusted comment",
            Some(STANDARD.encode(&text[..text.len() / 2])),
            test_key,
        ),
        // 48 base64 characters decode cleanly to a 36-byte signature, short of 74.
        row(
            "cut in the signature line",
            Some(STANDARD.encode(&text[..lines[0].len() + 1 + 48])),
            test_key,
        ),
        row(ANOTHER_KEY, Some(good.to_string()), other_key),
        row("missing", None, test_key),
        row("empty", Some(String::new()), test_key),
        row("not base64", Some("not base64!".into()), test_key),
        row(
            "not utf8",
            Some(STANDARD.encode([0xff, 0xfe, 0xfd])),
            test_key,
        ),
    ]
}
