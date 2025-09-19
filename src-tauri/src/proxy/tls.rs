use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyper_rustls::HttpsConnectorBuilder;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig as RustlsServerConfig;
use tokio_rustls::TlsAcceptor;

// use proxy_log! macro directly if needed

pub(crate) fn resolve_mitm_flags() -> (bool, bool) {
    let force_mitm = std::env::var("FORCE_MITM")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let sys_mitm = match crate::ca::is_ca_installed_in_system_trust() {
        Ok(v) => v,
        Err(_) => false,
    };
    (force_mitm, sys_mitm)
}

pub(crate) fn build_mitm_acceptor(host: &str) -> Result<TlsAcceptor, String> {
    let (ca_pem, ca_key_pem) = crate::ca::ensure_ca_exists()?;
    let (leaf_der, key_der, ca_der) = crate::ca::generate_leaf_cert_for_host(host, &ca_pem, &ca_key_pem)?;
    let certs = vec![CertificateDer::from(leaf_der), CertificateDer::from(ca_der)];
    let pkcs8_owned: PrivatePkcs8KeyDer<'static> = PrivatePkcs8KeyDer::from(key_der.clone());
    let priv_key = PrivateKeyDer::Pkcs8(pkcs8_owned);
    let mut server_cfg = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, priv_key)
        .map_err(|e| e.to_string())?;
    let disable_h2 = std::env::var("DISABLE_H2")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    server_cfg.alpn_protocols = if disable_h2 { vec![b"http/1.1".to_vec()] } else { vec![b"h2".to_vec(), b"http/1.1".to_vec()] };
    server_cfg.max_fragment_size = None;
    Ok(TlsAcceptor::from(std::sync::Arc::new(server_cfg)))
}

pub(crate) fn build_https_client() -> Client<hyper_rustls::HttpsConnector<HttpConnector>, crate::proxy::ProxyBody> {
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("native roots")
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}


