//! Wake-on-LAN "magic packet": 6 x 0xFF followed by the MAC address 16 times, sent by UDP broadcast.
//! Works when the Mac is on the same home network as the PC (Tailscale cannot carry broadcasts).

use std::net::UdpSocket;

pub fn parse_mac(mac: &str) -> Option<[u8; 6]> {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut p = vec![0xFFu8; 6];
    for _ in 0..16 {
        p.extend_from_slice(&mac);
    }
    p
}

pub fn wake(mac: &str) -> std::io::Result<()> {
    let mac = parse_mac(mac).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad MAC address"))?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    let pkt = magic_packet(mac);
    for port in [9u16, 7] {
        sock.send_to(&pkt, ("255.255.255.255", port))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_shape() {
        let mac = parse_mac("AA:bb-cc:dd:ee:01").unwrap();
        assert_eq!(mac, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01]);
        let p = magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert_eq!(&p[6..12], &mac);
        assert_eq!(&p[96..102], &mac);
        assert!(parse_mac("nope").is_none());
    }
}
