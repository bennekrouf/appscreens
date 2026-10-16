//! ShipScreen Pro: the licence on this computer.
//!
//! mayorana.ch signs each licence when it is bought, with a private Ed25519
//! key; the app holds the public half and checks the signature, so there is no
//! account, activation or network call. The key a buyer pastes is
//!
//! ```text
//! <base64url(payload JSON)>.<base64url(signature of those bytes)>
//! ```
//!
//! with the payload `{"v":1,"id","product","edition","email","issued","updates_until"}`
//! (the same format as GitAgent's, TrackCut's and SpreadRadar's). A licence
//! unlocks every release dated up to `updates_until`, and keeps unlocking those
//! releases after that day.
//!
//! Without Pro, ShipScreen works on one app (project folder); `pro.rs` keeps
//! which one. This module only says which status the app is in.

use std::path::PathBuf;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// The product id in every ShipScreen licence.
pub const PRODUCT: &str = "shipscreen";

/// Where "Buy ShipScreen Pro" leads.
pub const BUY_URL: &str = "https://mayorana.ch/en/apps/shipscreen";

/// The public half of mayorana.ch's licence signing key (32 bytes, standard
/// base64), built in by the release workflow from `SHIPSCREEN_LICENSE_PUBLIC_KEY`.
const PUBLIC_KEY: Option<&str> = option_env!("SHIPSCREEN_LICENSE_PUBLIC_KEY");

/// This build's release date (`YYYY-MM-DD`), set by build.rs. Empty when
/// unknown, which every licence covers.
const RELEASE_DATE: &str = env!("SHIPSCREEN_RELEASE_DATE");

const LICENCE_FILE: &str = "licence";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct License {
    pub id: String,
    pub product: String,
    pub edition: String,
    pub email: String,
    pub issued: String,
    pub updates_until: String,
}

impl License {
    /// ISO dates compare correctly as strings.
    pub fn covers(&self, release_date: &str) -> bool {
        release_date <= self.updates_until.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LicenseError {
    Malformed,
    BadSignature,
    OtherProduct(String),
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(
                f,
                "This isn't a complete licence key. Copy the whole key from the email."
            ),
            Self::BadSignature => write!(f, "This licence key isn't valid."),
            Self::OtherProduct(p) => write!(f, "This is a licence for {p}, not ShipScreen."),
        }
    }
}

/// The licence in `key`, if `public_key` signed it and it is for ShipScreen.
/// Whitespace is ignored: keys pasted from an email are often wrapped.
pub fn verify(key: &str, public_key: &[u8; 32]) -> Result<License, LicenseError> {
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    let (body, signature) = key.split_once('.').ok_or(LicenseError::Malformed)?;
    let payload = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| LicenseError::Malformed)?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| LicenseError::Malformed)?;
    let signature = Signature::from_slice(&signature).map_err(|_| LicenseError::Malformed)?;
    let verifying = VerifyingKey::from_bytes(public_key).map_err(|_| LicenseError::BadSignature)?;
    verifying
        .verify_strict(&payload, &signature)
        .map_err(|_| LicenseError::BadSignature)?;
    let license: License = serde_json::from_slice(&payload).map_err(|_| LicenseError::Malformed)?;
    if license.product != PRODUCT {
        return Err(LicenseError::OtherProduct(license.product));
    }
    Ok(license)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// No licence on this computer.
    Free,
    /// Licensed, and this build is covered.
    Pro(License),
    /// Licensed, but this build was released after the licence's updates ended.
    Renew(License),
    /// A build without the public key (a local build): licences can't be
    /// checked, and nothing is limited.
    Unavailable,
}

impl Status {
    /// Whether the free version's limit is lifted. A build that can't check
    /// licences isn't limited: that is a build from source, or a release
    /// missing its key, and neither should lock out someone who paid.
    pub fn unlimited(&self) -> bool {
        matches!(self, Status::Pro(_) | Status::Unavailable)
    }
}

fn public_key() -> Option<[u8; 32]> {
    STANDARD.decode(PUBLIC_KEY?.trim()).ok()?.try_into().ok()
}

/// The folder the licence (and the free version's choices) are kept in.
pub fn dir() -> PathBuf {
    // Beside settings.json, under the name the app had when it was first
    // written (see `global_config_path` in main.rs).
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("appscreens")
}

fn licence_path() -> PathBuf {
    dir().join(LICENCE_FILE)
}

fn status_of(license: License) -> Status {
    if license.covers(RELEASE_DATE) {
        Status::Pro(license)
    } else {
        Status::Renew(license)
    }
}

/// The licence saved on this computer, checked again every time: a key that
/// no longer verifies counts as none.
pub fn current() -> Status {
    let Some(public) = public_key() else {
        return Status::Unavailable;
    };
    let Ok(key) = std::fs::read_to_string(licence_path()) else {
        return Status::Free;
    };
    match verify(&key, &public) {
        Ok(l) => status_of(l),
        Err(_) => Status::Free,
    }
}

/// Checks `key` and, if it is an ShipScreen licence, saves it.
pub fn activate(key: &str) -> Result<Status, String> {
    let public = public_key()
        .ok_or("This build of ShipScreen can't check licences. Download it from mayorana.ch.")?;
    let license = verify(key, &public).map_err(|e| e.to_string())?;
    let key: String = key.chars().filter(|c| !c.is_whitespace()).collect();
    let path = licence_path();
    let save = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        // Written beside the target and renamed over it, so a crash can't leave
        // half a key behind.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, key.as_bytes())?;
        std::fs::rename(&tmp, &path)
    };
    save().map_err(|e| format!("The licence couldn't be saved: {e}"))?;
    Ok(status_of(license))
}

/// Removes the licence from this computer (to move it to another one).
pub fn deactivate() {
    let _ = std::fs::remove_file(licence_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn sign(json: &str, key: &SigningKey) -> String {
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(json),
            URL_SAFE_NO_PAD.encode(key.sign(json.as_bytes()).to_bytes())
        )
    }

    const PAYLOAD: &str = r#"{"v":1,"id":"lic_1","product":"shipscreen","edition":"pro","email":"dev@example.ch","issued":"2026-10-16","updates_until":"2027-10-16"}"#;

    #[test]
    fn a_signed_key_is_accepted_even_when_wrapped() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let key = sign(PAYLOAD, &signer);
        let (a, b) = key.split_at(30);
        let l = verify(&format!(" {a}\n{b} "), &signer.verifying_key().to_bytes()).unwrap();
        assert_eq!(l.email, "dev@example.ch");
        assert!(l.covers("2027-10-16"));
        assert!(!l.covers("2027-10-17"));
    }

    #[test]
    fn forged_edited_and_other_product_keys_are_refused() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let public = signer.verifying_key().to_bytes();
        let forger = SigningKey::from_bytes(&[8u8; 32]);
        assert_eq!(
            verify(&sign(PAYLOAD, &forger), &public),
            Err(LicenseError::BadSignature)
        );
        let (_, sig) = sign(PAYLOAD, &signer)
            .split_once('.')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap();
        let edited = URL_SAFE_NO_PAD.encode(PAYLOAD.replace("2027", "2099"));
        assert_eq!(
            verify(&format!("{edited}.{sig}"), &public),
            Err(LicenseError::BadSignature)
        );
        let other = sign(&PAYLOAD.replace("\"shipscreen\"", "\"gitagent\""), &signer);
        assert_eq!(
            verify(&other, &public),
            Err(LicenseError::OtherProduct("gitagent".into()))
        );
        assert_eq!(verify("nonsense", &public), Err(LicenseError::Malformed));
    }

    #[test]
    fn only_pro_and_keyless_builds_are_unlimited() {
        assert!(!Status::Free.unlimited());
        assert!(Status::Unavailable.unlimited());
        let l = License {
            id: "lic_1".into(),
            product: PRODUCT.into(),
            edition: "pro".into(),
            email: "dev@example.ch".into(),
            issued: "2026-10-16".into(),
            updates_until: "2027-10-16".into(),
        };
        assert!(Status::Pro(l.clone()).unlimited());
        // A licence whose updates ended keeps nothing unlocked for newer builds.
        assert!(!Status::Renew(l).unlimited());
    }
}
