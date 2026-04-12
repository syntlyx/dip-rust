/// Minimal built-in DNS server.
///
/// Resolves `*.{tld}` → 127.0.0.1 directly (no external process needed).
/// Everything else is forwarded to an upstream resolver (default: 8.8.8.8).
///
/// Wire format notes
/// -----------------
/// DNS packet layout (RFC 1035):
///   [0..2]  Transaction ID
///   [2..4]  Flags
///   [4..6]  QDCOUNT — question count
///   [6..8]  ANCOUNT — answer count
///   [8..10] NSCOUNT — authority count
///   [10..12] ARCOUNT — additional count
///   [12..]  Question section: <name> <qtype u16> <qclass u16>
///
/// A name is a sequence of length-prefixed labels terminated by 0x00.
/// A compressed pointer is 0xC0 followed by an offset byte.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;

const TTL: u32 = 60;

/// Starts the DNS server. Blocks until an error occurs.
pub async fn run(port: u16, tlds: Vec<String>, upstream: String) -> anyhow::Result<()> {
    let sock = Arc::new(
        UdpSocket::bind(("127.0.0.1", port))
            .await
            .map_err(|e| anyhow::anyhow!("Cannot bind DNS port {port}: {e}"))?,
    );
    eprintln!("dip-dns: :{port} — *.{} → 127.0.0.1", tlds.join(", *."));

    let mut buf = vec![0u8; 512];
    loop {
        let (len, client) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("dip-dns: recv error: {e}");
                continue;
            }
        };

        let data = buf[..len].to_vec();
        let sock = sock.clone();
        let tlds = tlds.clone();
        let upstream = upstream.clone();

        tokio::spawn(async move {
            let resp = if matches_tld(&data, &tlds) {
                make_response(&data)
            } else {
                forward(&data, &upstream).await.ok()
            };

            if let Some(r) = resp {
                let _ = sock.send_to(&r, client).await;
            }
        });
    }
}

// ─── matching ─────────────────────────────────────────────────────────────────

/// Returns true if the first question in the packet is for one of our TLDs.
fn matches_tld(data: &[u8], tlds: &[String]) -> bool {
    let Some((name, _)) = parse_first_question(data) else {
        return false;
    };
    let name = name.to_lowercase();
    tlds.iter()
        .any(|tld| name == *tld || name.ends_with(&format!(".{tld}")))
}

// ─── local response ───────────────────────────────────────────────────────────

/// Build a DNS response for our TLD:
///   A query    → answer with 127.0.0.1
///   AAAA query → NOERROR, no answers  (so OS falls back to A)
///   other      → NOERROR, no answers
fn make_response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }

    let (_, qtype) = parse_first_question(query)?;
    let q_end = question_section_end(query)?;

    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let answer_count: u16 = if qtype == 1 { 1 } else { 0 }; // 1 = A record type

    let mut resp = Vec::with_capacity(q_end + 16);

    // ── header ────────────────────────────────────────────────────────────────
    resp.extend_from_slice(&query[..2]); // Transaction ID
    resp.extend_from_slice(&[0x81, 0x80]); // Flags: QR=1 RD=1 RA=1
    resp.extend_from_slice(&qdcount.to_be_bytes());
    resp.extend_from_slice(&answer_count.to_be_bytes());
    resp.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    resp.extend_from_slice(&[0x00, 0x00]); // ARCOUNT

    // ── question section (copied verbatim) ────────────────────────────────────
    resp.extend_from_slice(&query[12..q_end]);

    // ── answer section (A records only) ───────────────────────────────────────
    if qtype == 1 {
        resp.extend_from_slice(&[0xC0, 0x0C]); // name pointer → offset 12
        resp.extend_from_slice(&[0x00, 0x01]); // type A
        resp.extend_from_slice(&[0x00, 0x01]); // class IN
        resp.extend_from_slice(&TTL.to_be_bytes());
        resp.extend_from_slice(&[0x00, 0x04]); // rdlength
        resp.extend_from_slice(&[127, 0, 0, 1]);
    }

    Some(resp)
}

// ─── forwarding ───────────────────────────────────────────────────────────────

/// Forward a query to the upstream resolver and return the raw response.
async fn forward(data: &[u8], upstream: &str) -> anyhow::Result<Vec<u8>> {
    let upstream_addr: SocketAddr = upstream.parse()?;
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.send_to(data, upstream_addr).await?;

    let mut buf = vec![0u8; 512];
    let (len, _) = tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf)).await??;

    Ok(buf[..len].to_vec())
}

// ─── DNS packet parsing ───────────────────────────────────────────────────────

/// Parse the name and qtype of the first question in a DNS packet.
fn parse_first_question(data: &[u8]) -> Option<(String, u16)> {
    if data.len() < 12 {
        return None;
    }
    let mut pos = 12usize;
    let name = read_name(data, &mut pos)?;
    if pos + 2 > data.len() {
        return None;
    }
    let qtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
    Some((name, qtype))
}

/// Returns the byte offset just after the question section.
fn question_section_end(data: &[u8]) -> Option<usize> {
    if data.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut pos = 12usize;
    for _ in 0..qdcount {
        skip_name(data, &mut pos)?;
        pos += 4; // qtype + qclass
        if pos > data.len() {
            return None;
        }
    }
    Some(pos)
}

/// Read a DNS name from `data` starting at `*pos`, advancing `*pos` past it.
fn read_name(data: &[u8], pos: &mut usize) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    loop {
        if *pos >= data.len() {
            return None;
        }
        let len = data[*pos] as usize;
        if len == 0 {
            *pos += 1;
            break;
        }
        if len >= 0xC0 {
            // Compressed pointer — skip, stop reading
            *pos += 2;
            break;
        }
        *pos += 1;
        if *pos + len > data.len() {
            return None;
        }
        let label = std::str::from_utf8(&data[*pos..*pos + len]).ok()?;
        labels.push(label.to_string());
        *pos += len;
    }
    Some(labels.join("."))
}

/// Advance `*pos` past a DNS name without decoding it.
fn skip_name(data: &[u8], pos: &mut usize) -> Option<()> {
    loop {
        if *pos >= data.len() {
            return None;
        }
        let len = data[*pos] as usize;
        if len == 0 {
            *pos += 1;
            return Some(());
        }
        if len >= 0xC0 {
            *pos += 2;
            return Some(());
        }
        *pos += len + 1;
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // A real DNS A-query for "backend.test" captured from dig
    // dig +norecurse backend.test A @127.0.0.1 -p 5354
    fn make_a_query(name: &str) -> Vec<u8> {
        let mut pkt = vec![
            0xAB, 0xCD, // ID
            0x01, 0x00, // Flags: RD=1
            0x00, 0x01, // QDCOUNT = 1
            0x00, 0x00, // ANCOUNT
            0x00, 0x00, // NSCOUNT
            0x00, 0x00, // ARCOUNT
        ];
        // Encode name
        for label in name.split('.') {
            pkt.push(label.len() as u8);
            pkt.extend_from_slice(label.as_bytes());
        }
        pkt.push(0x00); // root label
        pkt.extend_from_slice(&[0x00, 0x01]); // QTYPE A
        pkt.extend_from_slice(&[0x00, 0x01]); // QCLASS IN
        pkt
    }

    #[test]
    fn matches_test_tld() {
        let tlds = vec!["test".to_string()];
        let q = make_a_query("backend.test");
        assert!(matches_tld(&q, &tlds));
    }

    #[test]
    fn does_not_match_other_tlds() {
        let tlds = vec!["test".to_string()];
        let q = make_a_query("example.com");
        assert!(!matches_tld(&q, &tlds));
    }

    #[test]
    fn response_has_correct_ip() {
        let q = make_a_query("app.test");
        let r = make_response(&q).expect("response built");
        // Last 4 bytes of the response are the IP address
        assert_eq!(&r[r.len() - 4..], &[127, 0, 0, 1]);
    }

    #[test]
    fn aaaa_query_returns_empty_answer() {
        let mut q = make_a_query("app.test");
        // Change QTYPE to AAAA (28 = 0x001C)
        let qtype_pos = q.len() - 4;
        q[qtype_pos] = 0x00;
        q[qtype_pos + 1] = 0x1C;
        let r = make_response(&q).expect("response built");
        // ANCOUNT should be 0
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);
    }
}
