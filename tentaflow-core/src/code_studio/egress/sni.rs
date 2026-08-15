// ===== File: code_studio/egress/sni.rs — reading the server name out of a ClientHello =====
//
// A `CONNECT` names the destination in cleartext; the TLS handshake that
// follows names it again in the SNI extension. Only the second one reaches the
// server, so unless the two agree the tunnel goes somewhere the gateway never
// approved. Reading the SNI is the only way to compare them — the gateway does
// not terminate TLS and sees nothing else.
//
// The parser is length-driven and never trusts a length field past the end of
// the buffer. It answers `NeedMore` while the ClientHello is still arriving, so
// the proxy can keep reading with a bound instead of guessing a fixed size.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SniScan {
    Found(String),
    /// A complete ClientHello without a server_name extension.
    Absent,
    /// Not enough bytes yet; read more and scan again.
    NeedMore,
    /// Not a TLS ClientHello, or a malformed one.
    Malformed,
}

/// Scans the bytes a client sent immediately after `200 Connection
/// Established`. Handshake messages may be split across records, so records are
/// reassembled before the message is parsed.
pub fn scan_client_hello(buf: &[u8]) -> SniScan {
    let mut handshake = Vec::new();
    let mut offset = 0usize;
    loop {
        if buf.len() < offset + 5 {
            return if handshake.is_empty() {
                SniScan::NeedMore
            } else {
                scan_handshake(&handshake)
            };
        }
        // A handshake record starts with 0x16; anything else on a tunnel we
        // just approved for TLS is not a handshake we can verify.
        if buf[offset] != 0x16 {
            return SniScan::Malformed;
        }
        let length = u16::from_be_bytes([buf[offset + 3], buf[offset + 4]]) as usize;
        if length == 0 || length > 16 * 1024 {
            return SniScan::Malformed;
        }
        let body_start = offset + 5;
        let body_end = body_start + length;
        if buf.len() < body_end {
            handshake.extend_from_slice(&buf[body_start..]);
            return scan_handshake(&handshake);
        }
        handshake.extend_from_slice(&buf[body_start..body_end]);
        offset = body_end;
        match scan_handshake(&handshake) {
            SniScan::NeedMore if offset < buf.len() => continue,
            other => return other,
        }
    }
}

fn scan_handshake(handshake: &[u8]) -> SniScan {
    let mut reader = Reader::new(handshake);
    let msg_type = match reader.u8() {
        Some(value) => value,
        None => return SniScan::NeedMore,
    };
    if msg_type != 0x01 {
        return SniScan::Malformed;
    }
    let length = match reader.u24() {
        Some(value) => value,
        None => return SniScan::NeedMore,
    };
    let body = match reader.take(length) {
        Some(body) => body,
        // The declared body has not arrived yet. A length that is simply wrong
        // also lands here and stalls until the read bound in the proxy expires,
        // which is the safe direction: no verification, no tunnel.
        None => return SniScan::NeedMore,
    };
    parse_client_hello_body(body)
}

fn parse_client_hello_body(body: &[u8]) -> SniScan {
    let mut reader = Reader::new(body);
    if reader.take(2).is_none() {
        return SniScan::Malformed;
    }
    if reader.take(32).is_none() {
        return SniScan::Malformed;
    }
    let session_id_len = match reader.u8() {
        Some(value) => value as usize,
        None => return SniScan::Malformed,
    };
    if reader.take(session_id_len).is_none() {
        return SniScan::Malformed;
    }
    let cipher_len = match reader.u16() {
        Some(value) => value,
        None => return SniScan::Malformed,
    };
    if reader.take(cipher_len).is_none() {
        return SniScan::Malformed;
    }
    let compression_len = match reader.u8() {
        Some(value) => value as usize,
        None => return SniScan::Malformed,
    };
    if reader.take(compression_len).is_none() {
        return SniScan::Malformed;
    }
    let extensions_len = match reader.u16() {
        Some(value) => value,
        // TLS 1.2 permits a ClientHello with no extension block at all, which
        // means no SNI rather than a broken message.
        None => return SniScan::Absent,
    };
    let extensions = match reader.take(extensions_len) {
        Some(extensions) => extensions,
        None => return SniScan::Malformed,
    };

    let mut reader = Reader::new(extensions);
    while let Some(ext_type) = reader.u16() {
        let ext_len = match reader.u16() {
            Some(value) => value,
            None => return SniScan::Malformed,
        };
        let ext_body = match reader.take(ext_len) {
            Some(body) => body,
            None => return SniScan::Malformed,
        };
        if ext_type != 0x0000 {
            continue;
        }
        let mut names = Reader::new(ext_body);
        let list_len = match names.u16() {
            Some(value) => value,
            None => return SniScan::Malformed,
        };
        let list = match names.take(list_len) {
            Some(list) => list,
            None => return SniScan::Malformed,
        };
        let mut list = Reader::new(list);
        while let Some(name_type) = list.u8() {
            let name_len = match list.u16() {
                Some(value) => value,
                None => return SniScan::Malformed,
            };
            let name = match list.take(name_len) {
                Some(name) => name,
                None => return SniScan::Malformed,
            };
            if name_type != 0 {
                continue;
            }
            return match std::str::from_utf8(name) {
                Ok(host) if !host.is_empty() => SniScan::Found(host.to_ascii_lowercase()),
                _ => SniScan::Malformed,
            };
        }
        return SniScan::Absent;
    }
    SniScan::Absent
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let value = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(value)
    }

    fn u16(&mut self) -> Option<usize> {
        let hi = *self.buf.get(self.pos)? as usize;
        let lo = *self.buf.get(self.pos + 1)? as usize;
        self.pos += 2;
        Some((hi << 8) | lo)
    }

    fn u24(&mut self) -> Option<usize> {
        let a = *self.buf.get(self.pos)? as usize;
        let b = *self.buf.get(self.pos + 1)? as usize;
        let c = *self.buf.get(self.pos + 2)? as usize;
        self.pos += 3;
        Some((a << 16) | (b << 8) | c)
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal but structurally valid ClientHello record.
    fn client_hello(server_name: Option<&str>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version
        body.extend_from_slice(&[0x11; 32]); // random
        body.push(0); // session id
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // one cipher suite
        body.extend_from_slice(&[0x01, 0x00]); // compression

        let mut extensions = Vec::new();
        if let Some(name) = server_name {
            let mut entry = Vec::new();
            entry.push(0x00); // host_name
            entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
            entry.extend_from_slice(name.as_bytes());

            let mut ext_body = Vec::new();
            ext_body.extend_from_slice(&(entry.len() as u16).to_be_bytes());
            ext_body.extend_from_slice(&entry);

            extensions.extend_from_slice(&[0x00, 0x00]);
            extensions.extend_from_slice(&(ext_body.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&ext_body);
        }
        // A second extension, so the walk has to skip something.
        extensions.extend_from_slice(&[0x00, 0x2b, 0x00, 0x03, 0x02, 0x03, 0x04]);
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);

        let mut handshake = vec![0x01];
        handshake.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        handshake.extend_from_slice(&body);

        let mut record = vec![0x16, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    #[test]
    fn the_server_name_is_read_out_of_a_client_hello() {
        let record = client_hello(Some("crates.io"));
        assert_eq!(
            scan_client_hello(&record),
            SniScan::Found("crates.io".to_string())
        );
    }

    #[test]
    fn a_handshake_still_arriving_asks_for_more_instead_of_guessing() {
        let record = client_hello(Some("crates.io"));
        for cut in [1, 4, 10, record.len() - 1] {
            assert_eq!(
                scan_client_hello(&record[..cut]),
                SniScan::NeedMore,
                "a {cut}-byte prefix was not treated as incomplete"
            );
        }
    }

    #[test]
    fn a_hello_without_a_server_name_is_absent_not_found() {
        assert_eq!(scan_client_hello(&client_hello(None)), SniScan::Absent);
    }

    #[test]
    fn anything_that_is_not_a_tls_handshake_is_malformed() {
        assert_eq!(
            scan_client_hello(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"),
            SniScan::Malformed
        );
        assert_eq!(
            scan_client_hello(b"SSH-2.0-OpenSSH_9.6\r\n"),
            SniScan::Malformed
        );
    }

    #[test]
    fn a_length_past_the_end_never_reads_out_of_bounds() {
        let mut record = client_hello(Some("crates.io"));
        let len = record.len();
        // Claim a much longer handshake than the record carries.
        record[6] = 0xff;
        record[7] = 0xff;
        assert_eq!(scan_client_hello(&record[..len]), SniScan::NeedMore);
    }
}
