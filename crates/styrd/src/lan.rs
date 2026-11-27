//! Local-network fallback for when Tailscale is down.
//!
//! Without Tailscale the network is not encrypted, so this listener speaks TLS
//! with a self-signed certificate made once per Mac. The phone learns the
//! certificate's SHA-256 over Tailscale (in `hello`) and accepts only that
//! certificate here, so someone else on the Wi-Fi can neither read the traffic
//! nor pose as the Mac. Pairing is not offered on this listener.

use anyhow::Context;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::server::TlsStream;

const CERT_FILE: &str = "lan-cert.der";
const KEY_FILE: &str = "lan-key.der";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The certificate and key the LAN listener serves.
#[derive(Clone)]
pub struct Identity {
    pub acceptor: TlsAcceptor,
    /// Lowercase hex SHA-256 of the certificate (DER), what the phone pins.
    pub fingerprint: String,
}

/// Load this Mac's certificate from `dir`, making one the first time.
pub fn identity(dir: &Path) -> anyhow::Result<Identity> {
    let (cert, key) = load_or_create(dir)?;
    let fingerprint = hex::encode(Sha256::digest(&cert));
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )?;
    Ok(Identity {
        acceptor: TlsAcceptor::from(Arc::new(config)),
        fingerprint,
    })
}

fn load_or_create(dir: &Path) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let (cp, kp) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cp), std::fs::read(&kp))
        && !c.is_empty()
        && !k.is_empty()
    {
        return Ok((c, k));
    }
    let made = rcgen::generate_simple_self_signed(vec!["styrman.local".to_owned()])
        .context("making the local-network certificate")?;
    let cert = made.cert.der().to_vec();
    let key = made.signing_key.serialize_der();
    std::fs::create_dir_all(dir)?;
    write_private(&kp, &key)?;
    std::fs::write(&cp, &cert)?;
    Ok((cert, key))
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

/// Private IPv4 addresses on Wi-Fi/Ethernet (`en*`): never tunnels, bridges to
/// virtual machines, or anything public.
pub fn lan_addrs() -> Vec<IpAddr> {
    let Ok(ifs) = if_addrs::get_if_addrs() else {
        return vec![];
    };
    let mut out: Vec<IpAddr> = ifs
        .into_iter()
        .filter(|i| i.name.starts_with("en"))
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) if lan_v4(v4) => Some(IpAddr::V4(v4)),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn lan_v4(ip: Ipv4Addr) -> bool {
    ip.is_private() && !ip.is_loopback()
}

/// A TCP listener that hands axum only connections that finished the TLS
/// handshake. Handshakes run in their own tasks, so a slow one holds up nobody.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<tokio::net::TcpStream>, SocketAddr)>,
    local: SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, acceptor: TlsAcceptor) -> std::io::Result<Self> {
        let local = tcp.local_addr()?;
        let (tx, rx) = mpsc::channel(16);
        let accept_task = tokio::spawn(async move {
            loop {
                let (stream, peer) = match tcp.accept().await {
                    Ok(c) => c,
                    Err(_) => {
                        // Out of file descriptors and the like: back off, don't spin.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let (tx, acceptor) = (tx.clone(), acceptor.clone());
                tokio::spawn(async move {
                    let hs = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream));
                    if let Ok(Ok(tls)) = hs.await {
                        let _ = tx.send((tls, peer)).await;
                    }
                });
            }
        });
        Ok(Self {
            rx,
            local,
            accept_task,
        })
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(c) => c,
            // The accept task only ends when aborted (on drop); wait forever.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_private_v4_counts() {
        assert!(lan_v4(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(lan_v4(Ipv4Addr::new(10, 0, 0, 5)));
        assert!(lan_v4(Ipv4Addr::new(172, 16, 3, 4)));
        assert!(!lan_v4(Ipv4Addr::new(100, 126, 1, 1))); // Tailscale
        assert!(!lan_v4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!lan_v4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn the_certificate_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = identity(dir.path()).unwrap();
        let b = identity(dir.path()).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 64);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
    }

    /// A client that accepts exactly the certificate with this SHA-256, as the phone does.
    #[derive(Debug)]
    struct Pinned(String);

    impl tokio_rustls::rustls::client::danger::ServerCertVerifier for Pinned {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &tokio_rustls::rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: tokio_rustls::rustls::pki_types::UnixTime,
        ) -> Result<
            tokio_rustls::rustls::client::danger::ServerCertVerified,
            tokio_rustls::rustls::Error,
        > {
            if hex::encode(Sha256::digest(end_entity)) == self.0 {
                Ok(tokio_rustls::rustls::client::danger::ServerCertVerified::assertion())
            } else {
                Err(tokio_rustls::rustls::Error::General(
                    "not the pinned certificate".into(),
                ))
            }
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &tokio_rustls::rustls::DigitallySignedStruct,
        ) -> Result<
            tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
            tokio_rustls::rustls::Error,
        > {
            let p = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider();
            tokio_rustls::rustls::crypto::verify_tls12_signature(
                m,
                c,
                d,
                &p.signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &tokio_rustls::rustls::DigitallySignedStruct,
        ) -> Result<
            tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
            tokio_rustls::rustls::Error,
        > {
            let p = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider();
            tokio_rustls::rustls::crypto::verify_tls13_signature(
                m,
                c,
                d,
                &p.signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<tokio_rustls::rustls::SignatureScheme> {
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    async fn get(addr: SocketAddr, pin: &str) -> std::io::Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let cfg = tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned(pin.to_owned())))
            .with_no_client_auth();
        let tcp = tokio::net::TcpStream::connect(addr).await?;
        let name = tokio_rustls::rustls::pki_types::ServerName::IpAddress(addr.ip().into());
        let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
            .connect(name, tcp)
            .await?;
        tls.write_all(b"GET /hi HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await?;
        let mut out = String::new();
        tls.read_to_string(&mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn serves_only_to_a_client_that_pins_this_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let id = identity(dir.path()).unwrap();
        let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let listener = TlsListener::new(tcp, id.acceptor.clone()).unwrap();
        let addr = axum::serve::Listener::local_addr(&listener).unwrap();
        let app = axum::Router::new().route("/hi", axum::routing::get(|| async { "hello" }));
        tokio::spawn(async move { axum::serve(listener, app).await });

        // A client that doesn't finish the handshake holds up nobody.
        let _stalled = tokio::net::TcpStream::connect(addr).await.unwrap();

        let ok = get(addr, &id.fingerprint).await.unwrap();
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.ends_with("hello"), "{ok}");
        assert!(get(addr, &"0".repeat(64)).await.is_err());
    }
}
