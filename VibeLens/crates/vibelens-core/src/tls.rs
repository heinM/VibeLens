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

pub(crate) fn ensure_provider() {
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
        TlsMode::VerifyFull => Ok(Some(Arc::new(
            ClientConfig::builder()
                .with_root_certificates(verify_full_roots(cfg)?)
                .with_no_client_auth(),
        ))),
    }
}

/// The trust anchors of `VerifyFull`: OS roots, the bundled RDS CA bundle when asked for, and the
/// profile's CA file. Errors when none is left.
pub(crate) fn verify_full_roots(cfg: &TlsConfig) -> Result<rustls::RootCertStore> {
    ensure_provider();
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
    Ok(roots)
}

/// The certificates behind [`verify_full_roots`], in DER, for a driver that builds its own
/// rustls config from a list of CA certificates (MySQL). Same sources, same errors.
pub(crate) fn verify_full_root_certs(cfg: &TlsConfig) -> Result<Vec<CertificateDer<'static>>> {
    ensure_provider();
    let mut out: Vec<CertificateDer<'static>> = Vec::new();
    let native = rustls_native_certs::load_native_certs();
    out.extend(native.certs);
    if cfg.use_rds_bundle {
        match RDS_BUNDLE_PATH.get() {
            Some(p) if p.exists() => out.extend(read_pem(p)?),
            _ => tracing::warn!(
                "useRdsBundle requested but RDS CA bundle not found; see README (scripts/fetch-rds-ca.sh)"
            ),
        }
    }
    if let Some(path) = &cfg.ca_cert_path {
        out.extend(read_pem(std::path::Path::new(path))?);
    }
    // Only certificates rustls accepts as trust anchors are handed on.
    let mut store = rustls::RootCertStore::empty();
    out.retain(|c| store.add(c.clone()).is_ok());
    if out.is_empty() {
        return Err(CoreError::Invalid(
            "no trusted CA certificates available".into(),
        ));
    }
    Ok(out)
}

fn read_pem(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>> {
    let data = std::fs::read(path)
        .map_err(|e| CoreError::Invalid(format!("cannot read CA file {}: {e}", path.display())))?;
    let mut out = Vec::new();
    for cert in CertificateDer::pem_slice_iter(&data) {
        out.push(
            cert.map_err(|e| CoreError::Invalid(format!("bad PEM in {}: {e}", path.display())))?,
        );
    }
    let mut probe = rustls::RootCertStore::empty();
    for c in &out {
        probe
            .add(c.clone())
            .map_err(|e| CoreError::Invalid(format!("bad CA cert in {}: {e}", path.display())))?;
    }
    Ok(out)
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
