//! Shared rustls client config for Postgres and Valkey.
//!
//! RDS/Aurora and ElastiCache server certs chain to Amazon RDS CAs that are **not** in OS
//! trust stores, so `use_rds_bundle` adds the RDS global bundle (shipped as a Tauri resource,
//! path supplied at startup via [`set_rds_bundle_path`]).

use std::sync::{Arc, OnceLock};

use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, pem::PemObject};

use crate::error::{CoreError, Result};
use crate::model::{TlsConfig, TlsMode};

static RDS_BUNDLE_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

pub fn set_rds_bundle_path(path: std::path::PathBuf) {
    let _ = RDS_BUNDLE_PATH.set(path);
}

fn ensure_provider() {
    // aws-lc-rs is what the AWS SDK already pulls in; install it once as the process default.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn load_pem_into(store: &mut rustls::RootCertStore, path: &std::path::Path) -> Result<usize> {
    let data = std::fs::read(path)
        .map_err(|e| CoreError::Invalid(format!("cannot read CA file {}: {e}", path.display())))?;
    let mut n = 0;
    for cert in CertificateDer::pem_slice_iter(&data) {
        let cert =
            cert.map_err(|e| CoreError::Invalid(format!("bad PEM in {}: {e}", path.display())))?;
        store
            .add(cert)
            .map_err(|e| CoreError::Invalid(format!("bad CA cert in {}: {e}", path.display())))?;
        n += 1;
    }
    Ok(n)
}

/// Build a client config, or `None` when TLS is disabled.
pub fn client_config(cfg: &TlsConfig) -> Result<Option<Arc<ClientConfig>>> {
    ensure_provider();
    match cfg.mode {
        TlsMode::Disable => Ok(None),
        TlsMode::Require => {
            let c = ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(insecure::NoVerify::new()))
                .with_no_client_auth();
            Ok(Some(Arc::new(c)))
        }
        TlsMode::VerifyFull => {
            let mut roots = rustls::RootCertStore::empty();
            let native = rustls_native_certs::load_native_certs();
            for cert in native.certs {
                let _ = roots.add(cert);
            }
            if cfg.use_rds_bundle {
                match RDS_BUNDLE_PATH.get() {
                    Some(p) if p.exists() => {
                        load_pem_into(&mut roots, p)?;
                    }
                    _ => tracing::warn!(
                        "useRdsBundle requested but RDS CA bundle not found; see README (scripts/fetch-rds-ca.sh)"
                    ),
                }
            }
            if let Some(path) = &cfg.ca_cert_path {
                load_pem_into(&mut roots, std::path::Path::new(path))?;
            }
            if roots.is_empty() {
                return Err(CoreError::Invalid(
                    "no trusted CA certificates available".into(),
                ));
            }
            Ok(Some(Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )))
        }
    }
}

mod insecure {
    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    #[derive(Debug)]
    pub struct NoVerify(rustls::crypto::WebPkiSupportedAlgorithms);

    impl NoVerify {
        pub fn new() -> Self {
            Self(rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms)
        }
    }

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(m, c, d, &self.0)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(m, c, d, &self.0)
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.supported_schemes()
        }
    }
}
