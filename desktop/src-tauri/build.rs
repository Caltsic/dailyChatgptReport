use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    if cfg!(windows) {
        // Windows examples run without a console window; embed the manifest only there.
        let manifest =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("examples/windows.manifest");
        println!("cargo:rustc-link-arg-examples=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-examples=/MANIFESTINPUT:{}", manifest.display());
        println!("cargo:rerun-if-changed={}", manifest.display());
    }
    if env::var_os("CARGO_FEATURE_LEADER").is_some() {
        use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let key = Sha256::digest(b"CodexDailyReport:leader-resource:v1:2026");
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        for name in ["personal.v3", "department.v2", "extract.v2", "merge.v1"] {
            let source = format!("../../prompts/{name}.txt");
            println!("cargo:rerun-if-changed={source}");
            let nonce_bytes = uuid::Uuid::new_v4();
            let nonce = &nonce_bytes.as_bytes()[..12];
            let encrypted = cipher.encrypt(Nonce::from_slice(nonce), fs::read(source).unwrap().as_ref()).unwrap();
            let mut data = nonce.to_vec();
            data.extend(encrypted);
            fs::write(out.join(format!("{name}.enc")), data).unwrap();
        }
    }
    tauri_build::build()
}
