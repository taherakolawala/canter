//! A small WebSocket client for the browser's local DevTools endpoint.
//!
//! The peer is a browser this process launched, reached over loopback, so the
//! client implements only what that endpoint needs: the HTTP Upgrade
//! handshake, masked client frames, and reading text frames including
//! fragmentation, ping and close. Nothing here is a general-purpose
//! WebSocket implementation.

use std::io::{Read, Write};

/// Frames larger than this are refused rather than buffered. DevTools
/// messages are JSON; the largest we ask for is a page's visible text.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xa;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsError {
    /// The peer closed the connection, or it was lost.
    Closed,
    Failed(String),
}

impl WsError {
    pub fn message(&self) -> String {
        match self {
            WsError::Closed => "The sign-in browser closed the connection.".into(),
            WsError::Failed(why) => why.clone(),
        }
    }
}

fn failed(why: &str) -> WsError {
    WsError::Failed(why.to_string())
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    let bits = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        let indices = [n >> 18 & 63, n >> 12 & 63, n >> 6 & 63, n & 63];
        for (i, index) in indices.iter().enumerate() {
            out.push(if i <= chunk.len() {
                ALPHABET[*index as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// A value that differs between connections. The mask exists to stop
/// intermediaries caching a frame's bytes; this connection never leaves the
/// loopback interface, so a time-seeded value is enough.
fn pseudo_random(seed: u64) -> u64 {
    let mut x = seed
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

/// The `Sec-WebSocket-Key` the handshake sends, and whose echo it checks.
pub fn new_key(seed: u64) -> String {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&pseudo_random(seed).to_be_bytes());
    bytes[8..].copy_from_slice(&pseudo_random(seed ^ 0x5DEECE66D).to_be_bytes());
    base64(&bytes)
}

pub fn accept_key(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64(&sha1(&input))
}

pub fn handshake_request(host: &str, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
}

/// Check a complete handshake response. Returns how many bytes the headers
/// used, so anything after them is the start of the first frame.
pub fn check_handshake(response: &[u8], key: &str) -> Result<Option<usize>, WsError> {
    let end = match response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
    {
        Some(end) => end,
        None if response.len() > 16 * 1024 => {
            return Err(failed("The browser's debugging endpoint sent no response."));
        }
        None => return Ok(None),
    };
    let head = String::from_utf8_lossy(&response[..end]).to_ascii_lowercase();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default();
    if !status.contains(" 101") {
        return Err(failed(
            "The browser's debugging endpoint refused the connection.",
        ));
    }
    let expected = accept_key(key).to_ascii_lowercase();
    let accepted = lines.any(|line| {
        line.strip_prefix("sec-websocket-accept:")
            .is_some_and(|v| v.trim() == expected)
    });
    if !accepted {
        return Err(failed("The browser's debugging endpoint is not usable."));
    }
    Ok(Some(end))
}

pub fn encode(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let length = payload.len();
    if length < 126 {
        out.push(0x80 | length as u8);
    } else if length <= u16::MAX as usize {
        out.push(0x80 | 126);
        out.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(length as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Incomplete,
    Frame {
        fin: bool,
        opcode: u8,
        payload: Vec<u8>,
        consumed: usize,
    },
}

pub fn decode(buf: &[u8]) -> Result<Decoded, WsError> {
    if buf.len() < 2 {
        return Ok(Decoded::Incomplete);
    }
    if buf[0] & 0x70 != 0 {
        return Err(failed("The sign-in browser sent an unsupported frame."));
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0f;
    let masked = buf[1] & 0x80 != 0;
    let short = (buf[1] & 0x7f) as usize;
    let (length, mut at) = match short {
        126 if buf.len() >= 4 => (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4),
        127 if buf.len() >= 10 => {
            let mut eight = [0u8; 8];
            eight.copy_from_slice(&buf[2..10]);
            (u64::from_be_bytes(eight) as usize, 10)
        }
        126 | 127 => return Ok(Decoded::Incomplete),
        n => (n, 2),
    };
    if length > MAX_FRAME {
        return Err(failed("The sign-in browser sent an oversized message."));
    }
    let mask = if masked {
        if buf.len() < at + 4 {
            return Ok(Decoded::Incomplete);
        }
        let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        at += 4;
        Some(mask)
    } else {
        None
    };
    if buf.len() < at + length {
        return Ok(Decoded::Incomplete);
    }
    let mut payload = buf[at..at + length].to_vec();
    if let Some(mask) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Decoded::Frame {
        fin,
        opcode,
        payload,
        consumed: at + length,
    })
}

/// A text-message connection. Reads return `None` when the underlying stream
/// times out, so the caller can keep its own deadline.
pub struct WebSocket<S> {
    stream: S,
    buffer: Vec<u8>,
    /// Opcode and bytes of a message still arriving in fragments.
    partial: Option<(u8, Vec<u8>)>,
    masks: u64,
}

impl<S: Read + Write> WebSocket<S> {
    /// Perform the handshake. `host` is the `Host:` header, `path` the
    /// DevTools endpoint path.
    pub fn start(mut stream: S, host: &str, path: &str) -> Result<Self, WsError> {
        let key = new_key(path.len() as u64);
        stream
            .write_all(handshake_request(host, path, &key).as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|_| failed("Could not talk to the sign-in browser."))?;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(end) = check_handshake(&buffer, &key)? {
                buffer.drain(..end);
                return Ok(WebSocket {
                    stream,
                    buffer,
                    partial: None,
                    masks: 0,
                });
            }
            match stream.read(&mut chunk) {
                Ok(0) => return Err(WsError::Closed),
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(e) if timed_out(&e) => continue,
                Err(_) => return Err(WsError::Closed),
            }
        }
    }

    /// A connection on an already-open stream, for tests that script reads.
    #[cfg(test)]
    fn on(stream: S) -> Self {
        WebSocket {
            stream,
            buffer: Vec::new(),
            partial: None,
            masks: 0,
        }
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        self.masks = self.masks.wrapping_add(1);
        let seed = pseudo_random(self.masks);
        let mask = (seed as u32).to_be_bytes();
        let frame = encode(opcode, payload, mask);
        self.stream
            .write_all(&frame)
            .and_then(|()| self.stream.flush())
            .map_err(|_| WsError::Closed)
    }

    pub fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.write_frame(OP_TEXT, text.as_bytes())
    }

    /// The next complete text message, or `None` if the stream had nothing to
    /// read before its timeout.
    pub fn recv(&mut self) -> Result<Option<String>, WsError> {
        loop {
            match self.take_message()? {
                Some(text) => return Ok(Some(text)),
                None => {
                    let mut chunk = [0u8; 8192];
                    match self.stream.read(&mut chunk) {
                        Ok(0) => return Err(WsError::Closed),
                        Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                        Err(e) if timed_out(&e) => return Ok(None),
                        Err(_) => return Err(WsError::Closed),
                    }
                }
            }
        }
    }

    /// Decode whatever is already buffered, answering pings along the way.
    fn take_message(&mut self) -> Result<Option<String>, WsError> {
        loop {
            let (fin, opcode, payload) = match decode(&self.buffer)? {
                Decoded::Incomplete => return Ok(None),
                Decoded::Frame {
                    fin,
                    opcode,
                    payload,
                    consumed,
                } => {
                    self.buffer.drain(..consumed);
                    (fin, opcode, payload)
                }
            };
            match opcode {
                OP_CLOSE => return Err(WsError::Closed),
                OP_PING => self.write_frame(OP_PONG, &payload)?,
                OP_PONG => {}
                OP_TEXT | OP_BINARY => {
                    if self.partial.is_some() {
                        return Err(failed("The sign-in browser sent frames out of order."));
                    }
                    if fin {
                        if opcode == OP_TEXT {
                            return Ok(Some(text_of(payload)?));
                        }
                    } else {
                        self.partial = Some((opcode, payload));
                    }
                }
                0 => {
                    let Some((first, mut bytes)) = self.partial.take() else {
                        return Err(failed("The sign-in browser sent frames out of order."));
                    };
                    if bytes.len() + payload.len() > MAX_FRAME {
                        return Err(failed("The sign-in browser sent an oversized message."));
                    }
                    bytes.extend_from_slice(&payload);
                    if fin {
                        if first == OP_TEXT {
                            return Ok(Some(text_of(bytes)?));
                        }
                    } else {
                        self.partial = Some((first, bytes));
                    }
                }
                _ => return Err(failed("The sign-in browser sent an unsupported frame.")),
            }
        }
    }
}

fn text_of(payload: Vec<u8>) -> Result<String, WsError> {
    String::from_utf8(payload).map_err(|_| failed("The sign-in browser sent invalid text."))
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::ErrorKind;

    #[test]
    fn sha1_and_base64_match_known_values() {
        assert_eq!(
            sha1(b"abc"),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ]
        );
        assert_eq!(sha1(b"")[..4], [0xda, 0x39, 0xa3, 0xee]);
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"sure."), "c3VyZS4=");
        // The example key and accept value from RFC 6455 section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn handshake_is_checked_against_the_key_sent() {
        let key = new_key(1);
        assert_eq!(key.len(), 24);
        assert_ne!(key, new_key(2));
        let request = handshake_request("127.0.0.1:9222", "/devtools/browser/x", &key);
        assert!(request.starts_with("GET /devtools/browser/x HTTP/1.1\r\n"));
        assert!(request.contains(&format!("Sec-WebSocket-Key: {key}\r\n")));

        let good = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\nextra",
            accept_key(&key)
        );
        assert_eq!(
            check_handshake(good.as_bytes(), &key).unwrap(),
            Some(good.len() - 5)
        );
        assert_eq!(check_handshake(b"HTTP/1.1 101 ", &key).unwrap(), None);
        let wrong = "HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: nope\r\n\r\n";
        assert!(check_handshake(wrong.as_bytes(), &key).is_err());
        let refused = "HTTP/1.1 403 Forbidden\r\n\r\n";
        assert!(check_handshake(refused.as_bytes(), &key).is_err());
    }

    #[test]
    fn client_frames_are_masked_and_length_coded() {
        let frame = encode(OP_TEXT, b"hi", [1, 2, 3, 4]);
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1], 0x82);
        assert_eq!(&frame[2..6], &[1, 2, 3, 4]);
        assert_eq!(&frame[6..], &[b'h' ^ 1, b'i' ^ 2]);
        assert_eq!(encode(OP_TEXT, &[0; 200], [0; 4])[1], 0x80 | 126);
        assert_eq!(encode(OP_TEXT, &[0u8; 70000], [0; 4])[1], 0x80 | 127);
    }

    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 | opcode } else { opcode }];
        if payload.len() < 126 {
            out.push(payload.len() as u8);
        } else {
            out.push(126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn decode_reports_incomplete_and_rejects_bad_frames() {
        let frame = server_frame(true, OP_TEXT, b"ok");
        assert_eq!(decode(&frame[..1]).unwrap(), Decoded::Incomplete);
        assert_eq!(decode(&frame[..3]).unwrap(), Decoded::Incomplete);
        match decode(&frame).unwrap() {
            Decoded::Frame {
                opcode,
                payload,
                consumed,
                ..
            } => {
                assert_eq!((opcode, consumed), (OP_TEXT, 4));
                assert_eq!(payload, b"ok");
            }
            other => panic!("{other:?}"),
        }
        assert!(decode(&[0x90, 0x00]).is_err());
        let huge = [0x81, 127, 0xff, 0, 0, 0, 0, 0, 0, 0];
        assert!(decode(&huge).is_err());
    }

    /// A stream that hands out scripted reads; `None` is a read timeout.
    struct FakeStream {
        reads: VecDeque<Option<Vec<u8>>>,
        pub written: Vec<u8>,
        /// Answer the first read with a handshake reply built from the key
        /// the client wrote.
        echo_handshake: bool,
    }

    impl FakeStream {
        fn new(reads: Vec<Option<Vec<u8>>>) -> Self {
            FakeStream {
                reads: reads.into(),
                written: Vec::new(),
                echo_handshake: false,
            }
        }

        fn handshake_reply(&self) -> Vec<u8> {
            let request = String::from_utf8_lossy(&self.written);
            let key = request
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap_or_default()
                .to_string();
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(&key)
            )
            .into_bytes()
        }
    }

    impl Read for FakeStream {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.echo_handshake {
                self.echo_handshake = false;
                let reply = self.handshake_reply();
                out[..reply.len()].copy_from_slice(&reply);
                return Ok(reply.len());
            }
            match self.reads.pop_front() {
                Some(Some(bytes)) => {
                    out[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(None) => Err(std::io::Error::new(ErrorKind::WouldBlock, "timeout")),
                None => Ok(0),
            }
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn connected(reads: Vec<Option<Vec<u8>>>) -> WebSocket<FakeStream> {
        WebSocket::on(FakeStream::new(reads))
    }

    #[test]
    fn start_sends_the_request_and_accepts_a_matching_echo() {
        // The reply has to echo the key this client just generated, so it is
        // built from what the client wrote.
        let mut stream = FakeStream::new(vec![]);
        stream.echo_handshake = true;
        let frame = server_frame(true, OP_TEXT, b"ready");
        stream.reads.push_back(Some(frame));
        let mut socket = WebSocket::start(stream, "127.0.0.1:9222", "/devtools/browser/x").unwrap();
        assert_eq!(socket.recv().unwrap().unwrap(), "ready");
    }

    #[test]
    fn messages_survive_fragmentation_split_reads_and_pings() {
        let text = server_frame(true, OP_TEXT, b"{\"id\":1}");
        let ping = server_frame(true, OP_PING, b"p");
        let first = server_frame(false, OP_TEXT, b"{\"a\":");
        let rest = server_frame(true, 0, b"2}");
        let mut socket = connected(vec![
            Some(text[..2].to_vec()),
            None,
            Some(text[2..].to_vec()),
            Some([ping, first].concat()),
            Some(rest),
            Some(server_frame(true, OP_CLOSE, b"")),
        ]);
        assert_eq!(socket.recv().unwrap(), None);
        assert_eq!(socket.recv().unwrap().unwrap(), "{\"id\":1}");
        assert_eq!(socket.recv().unwrap().unwrap(), "{\"a\":2}");
        // The ping was answered with a masked pong.
        let pong = socket
            .stream
            .written
            .iter()
            .rposition(|b| *b == 0x8a)
            .unwrap();
        assert_eq!(socket.stream.written[pong + 1] & 0x80, 0x80);
        assert_eq!(socket.recv(), Err(WsError::Closed));
    }

    #[test]
    fn send_text_masks_the_payload() {
        let mut socket = connected(vec![]);
        socket.send_text("hello").unwrap();
        let frame = socket.stream.written.clone();
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1] & 0x80, 0x80);
        assert!(!frame.windows(5).any(|w| w == b"hello"));
        match decode(&unmask(&frame)).unwrap() {
            Decoded::Frame { payload, .. } => assert_eq!(payload, b"hello"),
            other => panic!("{other:?}"),
        }
    }

    /// Turn a client frame back into an unmasked one so `decode` can read it.
    fn unmask(frame: &[u8]) -> Vec<u8> {
        let mask = [frame[2], frame[3], frame[4], frame[5]];
        let mut out = vec![frame[0], frame[1] & 0x7f];
        out.extend(frame[6..].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }
}
