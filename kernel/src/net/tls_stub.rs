//! No-TLS build adapter. The HTTP clients still compile and report a clear
//! error for HTTPS URLs; enable the `net_tls` Cargo feature to include the
//! certificate-verifying TLS 1.3 implementation.

use core::marker::PhantomData;
use embedded_io::{Error, ErrorKind, ErrorType, Read, Write};

#[derive(Debug, Clone, Copy)]
pub struct TlsUnavailable;

impl Error for TlsUnavailable {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Unsupported
    }
}

impl core::fmt::Display for TlsUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("TLS is not enabled")
    }
}

impl core::error::Error for TlsUnavailable {}

pub fn load_default_root(_out: &mut [u8]) -> Result<&[u8], &'static str> {
    Err("TLS is not enabled in this kernel build (enable net_tls)")
}

pub struct TlsStream<'a> {
    _lifetime: PhantomData<&'a mut [u8]>,
}

impl<'a> TlsStream<'a> {
    pub fn connect(
        _host: &'a str,
        _remote_ip: [u8; 4],
        _remote_port: u16,
        _root_der: &'a [u8],
        _read_buffer: &'a mut [u8],
        _write_buffer: &'a mut [u8],
    ) -> Result<Self, TlsUnavailable> {
        Err(TlsUnavailable)
    }

    pub fn connect_with_timeout(
        _host: &'a str,
        _remote_ip: [u8; 4],
        _remote_port: u16,
        _root_der: &'a [u8],
        _read_buffer: &'a mut [u8],
        _write_buffer: &'a mut [u8],
        _timeout_ms: u64,
    ) -> Result<Self, TlsUnavailable> {
        Err(TlsUnavailable)
    }
}

impl ErrorType for TlsStream<'_> {
    type Error = TlsUnavailable;
}

impl Read for TlsStream<'_> {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
        Err(TlsUnavailable)
    }
}

impl Write for TlsStream<'_> {
    fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
        Err(TlsUnavailable)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Err(TlsUnavailable)
    }
}
