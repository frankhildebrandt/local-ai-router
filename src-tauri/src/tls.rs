use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug, Clone)]
pub struct TlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint: String,
}

impl TlsMaterial {
    pub fn server_config(&self) -> anyhow::Result<ServerConfig> {
        let certs = rustls_pemfile::certs(&mut self.cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .context("parsing TLS certificate")?;
        let key = rustls_pemfile::private_key(&mut self.key_pem.as_bytes())
            .context("parsing TLS private key")?
            .context("TLS private key is missing")?;
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("building TLS server config")
    }
}

pub fn resolve_tls_material(
    data_dir: &Path,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> anyhow::Result<TlsMaterial> {
    resolve_tls_material_for(data_dir, cert_path, key_path, &[])
}

pub fn resolve_tls_material_for(
    data_dir: &Path,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    extra_ips: &[IpAddr],
) -> anyhow::Result<TlsMaterial> {
    match (cert_path, key_path) {
        (Some(cert), Some(key)) => load_pem(cert, key),
        (None, None) => {
            let dir = data_dir.join("tls");
            let cert = dir.join("server.crt");
            let key = dir.join("server.key");
            if cert.is_file() && key.is_file() {
                load_pem(&cert, &key)
            } else {
                let material = generate_self_signed_for(extra_ips)?;
                persist_generated(&dir, &cert, &key, &material)?;
                Ok(material)
            }
        }
        _ => anyhow::bail!("both a certificate and a private key must be provided"),
    }
}

pub fn generate_self_signed() -> anyhow::Result<TlsMaterial> {
    generate_self_signed_for(&[])
}

pub fn generate_self_signed_for(extra_ips: &[IpAddr]) -> anyhow::Result<TlsMaterial> {
    let mut params = CertificateParams::new(vec!["localhost".into()])?;
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "Local AI Router");
    let mut sans = vec![
        SanType::DnsName("localhost".try_into()?),
        SanType::IpAddress(IpAddr::from([127, 0, 0, 1])),
        SanType::IpAddress(IpAddr::from([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ])),
    ];
    for ip in extra_ips {
        if !ip.is_unspecified() && !ip.is_loopback() {
            sans.push(SanType::IpAddress(*ip));
        }
    }
    params.subject_alt_names = sans;
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    let cert_pem = cert.pem();
    let key_pem = key.serialize_pem();
    let fingerprint = fingerprint_pem(&cert_pem)?;
    Ok(TlsMaterial {
        cert_pem,
        key_pem,
        fingerprint,
    })
}

fn persist_generated(
    dir: &Path,
    cert: &Path,
    key: &Path,
    material: &TlsMaterial,
) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    write_private(cert, material.cert_pem.as_bytes())?;
    write_private(key, material.key_pem.as_bytes())?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

fn load_pem(cert: &Path, key: &Path) -> anyhow::Result<TlsMaterial> {
    let cert_pem = fs::read_to_string(cert)
        .with_context(|| format!("reading TLS certificate {}", cert.display()))?;
    let key_pem = fs::read_to_string(key)
        .with_context(|| format!("reading TLS private key {}", key.display()))?;
    let fingerprint = fingerprint_pem(&cert_pem)?;
    Ok(TlsMaterial {
        cert_pem,
        key_pem,
        fingerprint,
    })
}

pub fn fingerprint_pem(cert_pem: &str) -> anyhow::Result<String> {
    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("parsing TLS certificate for fingerprint")?;
    let der = certs.first().context("TLS certificate is empty")?;
    Ok(format_fingerprint(&Sha256::digest(der.as_ref())))
}

pub fn format_fingerprint(digest: &[u8]) -> String {
    digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn parse_fingerprint(value: &str) -> anyhow::Result<[u8; 32]> {
    let hex: String = value
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    anyhow::ensure!(hex.len() == 64, "TLS fingerprint must be a SHA-256 digest");
    let mut digest = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
        digest[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)
            .context("TLS fingerprint is not valid hex")?;
    }
    Ok(digest)
}

pub fn pinned_client_config(fingerprint: &str) -> anyhow::Result<ClientConfig> {
    let expected = parse_fingerprint(fingerprint)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("building pinned TLS client")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier { expected, provider }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

#[derive(Debug)]
struct FingerprintVerifier {
    expected: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = Sha256::digest(end_entity.as_ref());
        if bool::from(digest.ct_eq(&self.expected)) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("TLS fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub struct TlsListener {
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl TlsListener {
    pub fn new(listener: tokio::net::TcpListener, config: Arc<ServerConfig>) -> Self {
        Self {
            listener,
            acceptor: tokio_rustls::TlsAcceptor::from(config),
        }
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue,
            };
            match self.acceptor.accept(stream).await {
                Ok(tls) => return (tls, addr),
                Err(_) => continue,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindConfig {
    pub ip: std::net::IpAddr,
    pub tls_required: bool,
}

pub fn bind_config(mode: &str, address: Option<&str>) -> anyhow::Result<BindConfig> {
    match mode {
        "" | "loopback" => Ok(BindConfig {
            ip: IpAddr::from([127, 0, 0, 1]),
            tls_required: false,
        }),
        "lan" => Ok(BindConfig {
            ip: IpAddr::from([0, 0, 0, 0]),
            tls_required: true,
        }),
        "address" => {
            let raw = address.context("bind address is required")?.trim();
            let ip: IpAddr = raw.parse().context("invalid bind address")?;
            Ok(BindConfig {
                ip,
                tls_required: !ip.is_loopback(),
            })
        }
        other => anyhow::bail!("unknown bind mode {other}"),
    }
}

pub fn user_cert_paths(cert: Option<&str>, key: Option<&str>) -> Option<(PathBuf, PathBuf)> {
    match (
        cert.filter(|value| !value.is_empty()),
        key.filter(|value| !value.is_empty()),
    ) {
        (Some(cert), Some(key)) => Some((PathBuf::from(cert), PathBuf::from(key))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_bind_does_not_require_tls() {
        let config = bind_config("loopback", None).unwrap();
        assert!(config.ip.is_loopback());
        assert!(!config.tls_required);
    }

    #[test]
    fn lan_and_non_loopback_address_require_tls() {
        assert!(bind_config("lan", None).unwrap().tls_required);
        let config = bind_config("address", Some("192.168.1.10")).unwrap();
        assert_eq!(config.ip.to_string(), "192.168.1.10");
        assert!(config.tls_required);
        assert!(
            !bind_config("address", Some("127.0.0.1"))
                .unwrap()
                .tls_required
        );
    }

    #[test]
    fn generated_certificate_has_a_stable_sha256_fingerprint() {
        let material = generate_self_signed().unwrap();
        assert!(material.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(material.key_pem.contains("BEGIN"));
        assert_eq!(material.fingerprint.matches(':').count(), 31);
        assert_eq!(
            fingerprint_pem(&material.cert_pem).unwrap(),
            material.fingerprint
        );
        assert_eq!(
            parse_fingerprint(&material.fingerprint).unwrap().as_slice(),
            Sha256::digest(
                rustls_pemfile::certs(&mut material.cert_pem.as_bytes())
                    .next()
                    .unwrap()
                    .unwrap()
                    .as_ref()
            )
            .as_slice()
        );
        assert!(parse_fingerprint("not-a-fingerprint").is_err());
        material.server_config().unwrap();
    }

    #[test]
    fn missing_tls_material_is_generated_into_the_data_dir() {
        let root = tempfile::tempdir().unwrap();
        let first = resolve_tls_material(root.path(), None, None).unwrap();
        let second = resolve_tls_material(root.path(), None, None).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(root.path().join("tls/server.crt").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.path().join("tls/server.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn generated_certificate_includes_a_specific_bind_address() {
        let ip = IpAddr::from([192, 168, 1, 10]);
        let material = generate_self_signed_for(&[ip]).unwrap();
        material.server_config().unwrap();
        assert_eq!(material.fingerprint.matches(':').count(), 31);
    }
}
