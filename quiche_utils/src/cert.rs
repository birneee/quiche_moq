use std::fs;
use std::path::PathBuf;
use boring::base64;
use boring::pkey::{PKey, Private};
use boring::sha::sha256;
use boring::x509::X509;
use log::info;
use rcgen::{generate_simple_self_signed, CertifiedKey};

pub fn load_or_generate_keys(cert_path: &Option<PathBuf>, key_path: &Option<PathBuf>) -> (X509, PKey<Private>) {
    match (&cert_path, &key_path) {
        (Some(cert_path), Some(key_path)) => {
            let cert = fs::read(cert_path).unwrap();
            let key = fs::read(key_path).unwrap();
            let cert = X509::from_pem(&cert).unwrap();
            let key = PKey::private_key_from_pem(&key).unwrap();
            (cert, key)
        },
        (None, None) => {
            info!("generate self signed TLS certificate");
            let CertifiedKey { cert, key_pair } = generate_simple_self_signed(["quiche".to_string()].to_vec()).unwrap();
            let cert = X509::from_pem(cert.pem().as_bytes()).unwrap();
            let key = PKey::private_key_from_pem(key_pair.serialize_pem().as_bytes()).unwrap();
            let spki = sha256(&key_pair.public_key_der());
            info!("certificate spki: {}", base64::encode_block(&spki));
            (cert, key)
        }
        _ => panic!("either provide key and certificate or neither of them")
    }
}