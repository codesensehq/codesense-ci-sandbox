// SPDX-License-Identifier: MIT
//! A key's passphrase goes straight from a system dialog into the keychain.
//! The webview never sees it: the app gets back only a keychain handle
//! (a [`CredentialRef`]), which is all that crosses IPC and all a binding
//! stores. At sign-in the askpass serves it from the keychain to `ssh`.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};

use crate::commands::credential_store::{CredentialStore, Secret};
use crate::domain::CredentialRef;

/// The keychain service holding SSH key passphrases, one entry per handle.
pub const SSH_SERVICE: &str = "codesense-ssh";

/// The `CredentialRef` provider of an SSH key passphrase.
pub const SSH_PROVIDER: &str = "ssh";

/// Ask for `key`'s passphrase in a system dialog: `None` when the person
/// cancelled. The passphrase comes back on the dialog's standard output —
/// never an argument, never a file.
#[cfg(target_os = "macos")]
pub fn prompt(key: &Path) -> Result<Option<Secret>, String> {
    let script = [
        "on run argv",
        "text returned of (display dialog (item 1 of argv) default answer \"\" with hidden answer \
         with title \"CodeSense\" buttons {\"Cancel\", \"Save to Keychain\"} \
         default button 2 cancel button 1 with icon caution)",
        "end run",
    ];
    let mut command = Command::new("/usr/bin/osascript");
    for line in script {
        command.args(["-e", line]);
    }
    let output = command
        .arg("--")
        .arg(format!(
            "Passphrase for {} — CodeSense keeps it in your keychain.",
            key.display()
        ))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("open the passphrase dialog: {e}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return if said.contains("-128") {
            Ok(None)
        } else {
            Err(format!("the passphrase dialog failed: {}", said.trim()))
        };
    }
    let mut text = String::from_utf8(output.stdout).map_err(|_| "the passphrase is not text")?;
    if text.ends_with('\n') {
        text.pop();
    }
    Ok((!text.is_empty()).then(|| Secret::new(text)))
}

#[cfg(not(target_os = "macos"))]
pub fn prompt(_key: &Path) -> Result<Option<Secret>, String> {
    Err("storing a key passphrase needs the macOS keychain".into())
}

/// Keep `passphrase` in the keychain under a fresh handle.
pub fn store(store: &dyn CredentialStore, passphrase: &Secret) -> Result<CredentialRef, String> {
    let handle = CredentialRef {
        provider: SSH_PROVIDER.into(),
        account: uuid::Uuid::new_v4().to_string(),
    };
    store.set(&handle.account, passphrase)?;
    Ok(handle)
}

/// The passphrase a handle names.
pub fn load(store: &dyn CredentialStore, handle: &CredentialRef) -> Result<Secret, String> {
    if handle.provider != SSH_PROVIDER {
        return Err(format!("{:?} is not an SSH passphrase", handle.provider));
    }
    store
        .get(&handle.account)?
        .ok_or_else(|| "The key's passphrase is no longer in your keychain. Save it again.".into())
}

/// Forget the passphrase a handle names.
pub fn forget(store: &dyn CredentialStore, handle: &CredentialRef) -> Result<(), String> {
    if handle.provider == SSH_PROVIDER {
        store.delete(&handle.account)
    } else {
        Ok(())
    }
}
