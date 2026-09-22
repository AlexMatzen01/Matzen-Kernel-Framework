//! TCP transport adapter for `embedded-tls`.
//!
//! This module deliberately contains no certificate or crypto policy. It only
//! adapts MFK's packet-pumped TCP API to the blocking `embedded-io` traits used
//! by the TLS client.

use alloc::vec::Vec;
use core::fmt;

use embedded_io::{Error, ErrorKind, ErrorType, Read, Write};
use embedded_tls::blocking::{
    Certificate, TlsClock, TlsConfig, TlsConnection, TlsContext, TlsVerifier,
};
use embedded_tls::{Aes128GcmSha256, CryptoProvider, TlsError};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

const TLS_HANDSHAKE_TIMEOUT_MS: u64 = 15_000;
const TCP_WRITE_CHUNK: usize = 1400;
const CERTIFICATE_SIZE: usize = 4096;

// ISRG Root X1, downloaded from letsencrypt.org/certs/isrgrootx1.pem.
// Keep the source as base64 so the repository remains text-only; it is
// decoded into caller-owned storage before constructing the verifier.
const ISRG_ROOT_X1_B64: &str = "MIIFazCCA1OgAwIBAgIRAIIQz7DSQONZRGPgu2OCiwAwDQYJKoZIhvcNAQELBQAwTzELMAkGA1UEBhMCVVMxKTAnBgNVBAoTIEludGVybmV0IFNlY3VyaXR5IFJlc2VhcmNoIEdyb3VwMRUwEwYDVQQDEwxJU1JHIFJvb3QgWDEwHhcNMTUwNjA0MTEwNDM4WhcNMzUwNjA0MTEwNDM4WjBPMQswCQYDVQQGEwJVUzEpMCcGA1UEChMgSW50ZXJuZXQgU2VjdXJpdHkgUmVzZWFyY2ggR3JvdXAxFTATBgNVBAMTDElTUkcgUm9vdCBYMTCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIBAK3oJHP0FDfzm54rVygch77ct984kIxuPOZXoHj3dcKi/vVqbvYATyjb3miGbESTtrFj/RQSa78f0uoxmyF+0TM8ukj13Xnfs7j/EvEhmkvBioZxaUpmZmyPfjxwv60pIgbz5MDmgK7iS4+3mX6UA5/TR5d8mUgjU+g4rk8Kb4Mu0UlXjIB0ttov0DiNewNwIRt18jA8+o+u3dpjq+sWT8KOEUt+zwvo/7V3LvSye0rgTBIlDHCNAymg4VMk7BPZ7hm/ELNKjD+Jo2FR3qyHB5T0Y3HsLuJvW5iB4YlcNHlsdu87kGJ55tukmi8mxdAQ4Q7e2RCOFvu396j3x+UCB5iPNgiV5+I3lg02dZ77DnKxHZu8A/lJBdiB3QW0KtZB6awBdpUKD9jf1b0SHzUvKBds0pjBqAlkd25HN7rOrFleaJ1/ctaJxQZBKT5ZPt0m9STJEadao0xAH0ahmbWnOlFuhjuefXKnEgV4We0+UXgVCwOPjdAvBbI+e0ocS3MFEvzG6uBQE3xDk3SzynTnjh8BCNAw1FtxNrQHusEwMFxIt4I7mKZ9YIqioymCzLq9gwQbooMDQaHWBfEbwrbwqHyGO0aoSCqI3Haadr8faqU9GY/rOPNk3sgrDQoo//fb4hVC1CLQJ13hef4Y53CIrU7m2Ys6xt0nUW7/vGT1M0NPAgMBAAGjQjBAMA4GA1UdDwEB/wQEAwIBBjAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBR5tFnme7bl5AFzgAiIyBpY9umbbjANBgkqhkiG9w0BAQsFAAOCAgEAVR9YqbyyqFDQDLHYGmkgJykIrGF1XIpu+ILlaS/V9lZLubhzEFnTIZd+50xx+7LSYK05qAvqFyFWhfFQDlnrzuBZ6brJFe+GnY+EgPbk6ZGQ3BebYhtF8GaV0nxvwuo77x/Py9auJ/GpsMiu/X1+mvoiBOv/2X/qkSsisRcOj/KKNFtY2PwByVS5uCbMiogziUwthDyC3+6WVwW6LLv3xLfHTjuCvjHIInNzktHCgKQ5ORAzI4JMPJ+GslWYHb4phowim57iaztXOoJwTdwJx4nLCgdNbOhdjsnvzqvHu7UrTkXWStAmzOVyyghqpZXjFaH3pO3JLF+l+/+sKAIuvtd7u+Nxe5AW0wdeRlN8NwdCjNPElpzVmbUq4JUagEiuTDkHzsxHpFKVK7q4+63SM1N95R1NbdWhscdCb+ZAJzVcoyi3B43njTOQ5yOf+1CceWxG1bQVs5ZufpsMljq4Ui0/1lvh+wjChP4kqKOJ2qxq4RgqsahDYVvTH9w7jXbyLeiNdd8XM2w9U/t7y0Ff/9yi0GE44Za4rF2LN9d11TPAmRGunUHBcnWEvgJBQl9nJEiU0Zsnvgc/ubhPgXRR4Xq37Z0j4r7g1SgEEzwxA57demyPxgcYxn/eR44/KJ4EBs+lVDR3veyJm+kXQ99b21/+jh5Xos1AnX5iItreGCc=";

pub fn load_default_root(out: &mut [u8]) -> Result<&[u8], &'static str> {
    let mut value = 0u32;
    let mut bits = 0u8;
    let mut written = 0;
    for byte in ISRG_ROOT_X1_B64.bytes() {
        if byte == b'=' {
            break;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        value = (value << 6) | digit;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if written == out.len() {
                return Err("TLS root certificate buffer too small");
            }
            out[written] = (value >> bits) as u8;
            written += 1;
            value &= (1 << bits) - 1;
        }
    }
    Ok(&out[..written])
}

/// MFK's RTC-backed TLS clock. A zero/uninitialized clock is rejected by the
/// verifier instead of silently accepting certificates without time checks.
pub struct MfkTlsClock;

impl TlsClock for MfkTlsClock {
    fn now() -> Option<u64> {
        if crate::time::is_initialized() {
            Some(crate::time::wall_epoch_secs())
        } else {
            None
        }
    }
}

/// Certificate-verifying provider for the selected TLS 1.3 cipher suite.
/// `root_der` must be an explicitly trusted DER-encoded CA certificate.
pub struct MfkTlsProvider<'a> {
    verifier: embedded_tls::pki::CertVerifier<'a, Aes128GcmSha256, MfkTlsClock, CERTIFICATE_SIZE>,
    rng: ChaCha20Rng,
}

/// An established TLS 1.3 stream over the MFK TCP stack.
pub struct TlsStream<'a> {
    inner: TlsConnection<'a, TcpSocket, Aes128GcmSha256>,
}

impl<'a> TlsStream<'a> {
    pub fn connect(
        host: &'a str,
        remote_ip: [u8; 4],
        remote_port: u16,
        root_der: &'a [u8],
        read_buffer: &'a mut [u8],
        write_buffer: &'a mut [u8],
    ) -> Result<Self, TlsError> {
        Self::connect_with_timeout(
            host,
            remote_ip,
            remote_port,
            root_der,
            read_buffer,
            write_buffer,
            TLS_HANDSHAKE_TIMEOUT_MS,
        )
    }

    pub fn connect_with_timeout(
        host: &'a str,
        remote_ip: [u8; 4],
        remote_port: u16,
        root_der: &'a [u8],
        read_buffer: &'a mut [u8],
        write_buffer: &'a mut [u8],
        timeout_ms: u64,
    ) -> Result<Self, TlsError> {
        crate::net_log!("TLS: opening TLS 1.3 session for {}", host);
        let socket = TcpSocket::connect_with_timeout(remote_ip, remote_port, timeout_ms)
            .map_err(|_| TlsError::ConnectionClosed)?;
        let config = TlsConfig::new().with_server_name(host);
        let provider = MfkTlsProvider::from_root(root_der)?;
        let mut inner = TlsConnection::new(socket, read_buffer, write_buffer);
        crate::net_log!("TLS: ClientHello sent / waiting for server flight");
        match inner.open(TlsContext::new(&config, provider)) {
            Ok(()) => crate::net_log!("TLS: handshake complete"),
            Err(error) => {
                crate::net_log!("TLS: handshake failed: {:?}", error);
                return Err(error);
            }
        }
        Ok(Self { inner })
    }
}

impl ErrorType for TlsStream<'_> {
    type Error = TlsError;
}

impl Read for TlsStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buf)
    }
}

impl Write for TlsStream<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl<'a> MfkTlsProvider<'a> {
    pub fn from_root(root_der: &'a [u8]) -> Result<Self, TlsError> {
        let mut seed = [0u8; 32];
        let mut hardware = super::entropy::Rdrand;
        rand_core::RngCore::try_fill_bytes(&mut hardware, &mut seed)
            .map_err(|_| TlsError::Unimplemented)?;
        Ok(Self {
            verifier: embedded_tls::pki::CertVerifier::new(Certificate::X509(root_der)),
            rng: ChaCha20Rng::from_seed(seed),
        })
    }
}

impl CryptoProvider for MfkTlsProvider<'_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = Vec<u8>;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpSocketError {
    Cancelled,
    ConnectionClosed,
    NotConnected,
    TimedOut,
    Network,
}

impl fmt::Display for TcpSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "operation cancelled",
            Self::ConnectionClosed => "connection closed",
            Self::NotConnected => "connection not established",
            Self::TimedOut => "operation timed out",
            Self::Network => "network operation failed",
        })
    }
}

impl core::error::Error for TcpSocketError {}

impl Error for TcpSocketError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Cancelled => ErrorKind::Interrupted,
            Self::ConnectionClosed => ErrorKind::ConnectionReset,
            Self::NotConnected => ErrorKind::NotConnected,
            Self::TimedOut => ErrorKind::TimedOut,
            Self::Network => ErrorKind::Other,
        }
    }
}

/// A blocking, packet-pumped TCP stream for TLS and other stream protocols.
pub struct TcpSocket {
    local_port: u16,
    pending: Vec<u8>,
    deadline_ms: u64,
}

impl TcpSocket {
    pub fn connect(remote_ip: [u8; 4], remote_port: u16) -> Result<Self, TcpSocketError> {
        Self::connect_with_timeout(remote_ip, remote_port, TLS_HANDSHAKE_TIMEOUT_MS)
    }

    fn connect_with_timeout(
        remote_ip: [u8; 4],
        remote_port: u16,
        timeout_ms: u64,
    ) -> Result<Self, TcpSocketError> {
        let local_port = super::http::connect_wait(remote_ip, remote_port, timeout_ms)
            .map_err(|_| TcpSocketError::Network)?;
        Ok(Self {
            local_port,
            pending: Vec::new(),
            deadline_ms: super::http::now_ms().saturating_add(timeout_ms),
        })
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    fn check_state(&self) -> Result<(), TcpSocketError> {
        match super::tcp::get_state(self.local_port) {
            Some(super::tcp::TcpState::Established) => Ok(()),
            Some(super::tcp::TcpState::CloseWait | super::tcp::TcpState::Closed) | None => {
                Err(TcpSocketError::ConnectionClosed)
            }
            _ => Err(TcpSocketError::NotConnected),
        }
    }

    fn fill_pending(&mut self) -> Result<(), TcpSocketError> {
        loop {
            if super::http::interrupted() {
                return Err(TcpSocketError::Cancelled);
            }
            super::http::pump();
            if let Some(data) = super::tcp::read_data(self.local_port) {
                crate::net_log!("TLS: TCP supplied {} bytes", data.len());
                self.pending.extend_from_slice(&data);
                return Ok(());
            }
            match super::tcp::get_state(self.local_port) {
                Some(super::tcp::TcpState::CloseWait | super::tcp::TcpState::Closed) | None => {
                    return Ok(())
                }
                _ => {}
            }
            if super::http::now_ms() >= self.deadline_ms {
                crate::net_log!("TLS: TCP read deadline expired");
                return Err(TcpSocketError::TimedOut);
            }
        }
    }
}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        super::http::cleanup(self.local_port);
    }
}

impl ErrorType for TcpSocket {
    type Error = TcpSocketError;
}

impl Read for TcpSocket {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pending.is_empty() {
            self.fill_pending()?;
        }
        if self.pending.is_empty() {
            return Ok(0);
        }
        let count = core::cmp::min(buf.len(), self.pending.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

impl Write for TcpSocket {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.check_state()?;
        let count = core::cmp::min(buf.len(), TCP_WRITE_CHUNK);
        super::tcp::send_data(self.local_port, &buf[..count])
            .map_err(|_| TcpSocketError::Network)?;
        crate::net_log!("TLS: sent {} bytes", count);
        super::http::pump();
        Ok(count)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.check_state()
    }
}
