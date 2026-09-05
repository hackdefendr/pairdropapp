//! TLS setup, shared by the `/config` fetch and the WebSocket.
//!
//! rustls rather than the platform's TLS stack: it needs no system libraries, so the
//! protocol and transport crates build identically on Linux and on the macOS machine
//! this is being developed on.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("could not build an HTTP client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("could not build the WebSocket request: {0}")]
    Request(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("invalid header value: {0}")]
    Header(#[from] tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue),
}

pub(crate) fn http_client(config: &super::SignalingConfig) -> Result<reqwest::Client, TlsError> {
    let builder = reqwest::Client::builder().user_agent(config.user_agent.clone());
    let builder = if config.allow_untrusted_tls {
        builder.danger_accept_invalid_certs(true)
    } else {
        builder
    };
    Ok(builder.build()?)
}

pub(crate) fn ws_connector(
    config: &super::SignalingConfig,
) -> Result<Arc<rustls::ClientConfig>, TlsError> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };

    let mut client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    if config.allow_untrusted_tls {
        client_config
            .dangerous()
            .set_certificate_verifier(Arc::new(AcceptAnyCertificate));
    }

    Ok(Arc::new(client_config))
}

/// The signaling server reads the device name from the `User-Agent` of the upgrade
/// request, so it has to be set on the handshake rather than on a later message.
pub(crate) fn ws_request(url: &url::Url, user_agent: &str) -> Result<Request, TlsError> {
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("User-Agent", user_agent.parse()?);
    Ok(request)
}

/// Verifies nothing. Reachable only when the user explicitly opts in for a self-hosted
/// instance behind a self-signed certificate, which is the same escape hatch the macOS
/// client offers — and it is exactly as dangerous there.
#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .map(|provider| provider.signature_verification_algorithms.supported_schemes())
            .unwrap_or_default()
    }
}
