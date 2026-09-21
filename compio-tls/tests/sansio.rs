//! Prototype check: drive `rustls` directly on compio's owned-buffer IO
//! traits, over a real `compio_net::TcpStream`, with no `futures::AsyncRead`
//! and no `futures_rustls` anywhere in the path.
#![cfg(feature = "rustls-sansio")]

use std::sync::Arc;

use compio_io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use compio_net::{TcpListener, TcpStream};
use compio_tls::sansio::TlsStream;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
};

fn certs() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let key = PrivateKeyDer::try_from(key.serialize_der()).unwrap();
    (cert.der().clone(), key)
}

#[compio_macros::test]
async fn sansio_handshake_and_echo() {
    let (cert, key) = certs();

    let server_config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap(),
    );

    let mut roots = RootCertStore::empty();
    roots.add(cert).unwrap();
    let client_config = Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = compio_runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let conn = ServerConnection::new(server_config).unwrap();
        let mut tls = TlsStream::new(stream, conn);

        tls.handshake().await.unwrap();

        // Echo one message back, all through compio's owned-buffer traits.
        let (n, buf) = tls.read(Vec::with_capacity(64)).await.unwrap();
        assert_eq!(&buf[..n], b"hello sans-io");
        tls.write_all(buf).await.unwrap();
        tls.flush().await.unwrap();
        tls.shutdown().await.unwrap();
    });

    let stream = TcpStream::connect(addr).await.unwrap();
    let conn =
        ClientConnection::new(client_config, ServerName::try_from("localhost").unwrap()).unwrap();
    let mut tls = TlsStream::new(stream, conn);

    tls.handshake().await.unwrap();
    assert!(
        !tls.get_ref().peer_addr().is_err(),
        "underlying compio stream is still usable"
    );
    // Prove a real handshake happened rather than the test short-circuiting.
    assert_eq!(
        tls.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3),
        "negotiated TLS 1.3"
    );
    assert!(tls.peer_certificates_len() > 0, "server presented a cert");

    tls.write_all(b"hello sans-io".to_vec()).await.unwrap();
    tls.flush().await.unwrap();

    let (n, buf) = tls.read(Vec::with_capacity(64)).await.unwrap();
    assert_eq!(&buf[..n], b"hello sans-io");

    server.await.unwrap();
}
