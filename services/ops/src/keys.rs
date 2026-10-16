//! Signing keys: only from the paths given on the command line, and never printed.
//!
//! A key is a Solana CLI keypair file (`solana-keygen new -o <file>`). Hardware wallets
//! (`usb://ledger…`) are recognised and refused with an explanation: this build does not link
//! the remote-wallet stack (its USB/HID dependencies; M11 decides). Members who sign with a
//! Ledger approve and execute in the Squads app instead; this tool's `status` and `approve`
//! print what a transaction runs so it can be checked there.
//!
//! Nothing here ever prints or returns a secret: a file that does not parse is reported by its
//! path only (a parser message can quote the bytes it choked on), and the only thing the tool
//! prints of a key is its public half.

use std::path::Path;

use anyhow::bail;
use solana_keypair::{Keypair, read_keypair_file};
use solana_signer::Signer;

/// Loads the keypair at `path` (module docs). `what` names the role in errors.
pub fn load(path: &str, what: &str) -> anyhow::Result<Keypair> {
    if path.starts_with("usb://") {
        bail!(
            "{what}: hardware wallet paths ({path}) are not supported by this build (the \
             remote-wallet stack is not linked); approve or execute with the Squads app, or give \
             a keypair file"
        );
    }
    if path.starts_with("prompt:") || path == "-" || path == "stdin" || path == "ASK" {
        bail!("{what}: only keypair files are accepted ({path:?})");
    }
    if !Path::new(path).is_file() {
        bail!("{what}: no keypair file at {path}");
    }
    #[cfg(unix)]
    warn_if_readable_by_others(path, what);
    // The parser's message is dropped on purpose: it can quote the file's content.
    read_keypair_file(path)
        .map_err(|_| anyhow::anyhow!("{what}: {path} is not a keypair file (contents not shown)"))
}

#[cfg(unix)]
fn warn_if_readable_by_others(path: &str, what: &str) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(path)
        && m.permissions().mode() & 0o077 != 0
    {
        eprintln!(
            "warning: {what}: {path} is readable by other users (mode {:o}); chmod 600 it",
            m.permissions().mode() & 0o777
        );
    }
}

/// `key`'s public half, for messages.
pub fn public(key: &Keypair) -> String {
    key.pubkey().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shortwin-ops-keys-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn refuses_hardware_and_prompt_paths() {
        let e = load("usb://ledger?key=0", "--key").unwrap_err().to_string();
        assert!(e.contains("hardware wallet") && e.contains("--key"), "{e}");
        assert!(load("prompt://", "--key").is_err());
        assert!(load("-", "--key").is_err());
        assert!(
            load("/nonexistent/k.json", "--key")
                .unwrap_err()
                .to_string()
                .contains("no keypair file")
        );
    }

    /// A malformed key file is reported by its path, never by its content.
    #[test]
    fn a_bad_file_is_never_quoted() {
        let p = dir().join("bad.json");
        // 64 numbers but one out of range: serde_json's message would quote `300`.
        let mut v = vec!["17"; 63];
        v.push("300");
        std::fs::write(&p, format!("[{}]", v.join(","))).unwrap();
        let e = format!("{:#}", load(p.to_str().unwrap(), "--payer").unwrap_err());
        assert!(e.contains("contents not shown"), "{e}");
        assert!(!e.contains("300") && !e.contains("17,"), "{e}");
    }

    #[test]
    fn loads_a_keypair_file() {
        let k = Keypair::new();
        let p = dir().join("ok.json");
        solana_keypair::write_keypair_file(&k, &p).unwrap();
        let got = load(p.to_str().unwrap(), "--key").unwrap();
        assert_eq!(got.pubkey(), k.pubkey());
        assert_eq!(public(&got), k.pubkey().to_string());
    }
}
