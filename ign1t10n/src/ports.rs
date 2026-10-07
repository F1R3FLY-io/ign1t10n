//! The port plan (spec §6.2). Each node uses six ports at offsets +0..+5 from
//! its base: protocol, gRPC external, gRPC internal, HTTP, discovery, admin.
//! Embers uses one port.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

pub const BOOTSTRAP_BASE: u16 = 40400;
pub const OBSERVER_BASE: u16 = 40450;
pub const EMBERS_PORT: u16 = 40600;
/// F1R3Games (spec v0.4 §10.2): the portal at 40700, each game at
/// 40701, 40702, ... in a decade of its own (Decision 9).
pub const GAMES_BASE: u16 = 40700;
const GAMES_FALLBACK_START: u16 = 41700;
const FALLBACK_START: u16 = 41400;
const FALLBACK_END: u16 = 49990;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Block(pub u16);

impl Block {
    pub fn protocol(self) -> u16 { self.0 }
    pub fn grpc_external(self) -> u16 { self.0 + 1 }
    pub fn grpc_internal(self) -> u16 { self.0 + 2 }
    pub fn http(self) -> u16 { self.0 + 3 }
    pub fn discovery(self) -> u16 { self.0 + 4 }
    pub fn admin(self) -> u16 { self.0 + 5 }
    pub fn all(self) -> [u16; 6] { [self.0, self.0 + 1, self.0 + 2, self.0 + 3, self.0 + 4, self.0 + 5] }
    pub fn http_url(self) -> String { format!("http://127.0.0.1:{}", self.http()) }
}

/// The plan's base for validator `slot` (1-based): 40410..40440, then
/// 40460..40510, skipping the observer's decade.
pub fn validator_base(slot: u8) -> u16 {
    assert!((1..=crate::MAX_VALIDATORS).contains(&slot));
    if slot <= 4 { 40400 + 10 * slot as u16 } else { 40410 + 10 * slot as u16 }
}

/// Is `p` free on loopback? Binding alone is not enough on macOS: Rust sets
/// SO_REUSEADDR, and BSD then lets 127.0.0.1:p be bound while another
/// program (a Docker shard, say) listens on *:p, and connections to
/// 127.0.0.1:p can reach that other program. So a port is free only if we
/// can bind it *and* nothing answers a connection to it.
pub fn port_free(p: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    if TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, p)).is_err() {
        return false;
    }
    let answers = |a: SocketAddr| TcpStream::connect_timeout(&a, Duration::from_millis(200)).is_ok();
    !(answers(SocketAddr::from((Ipv4Addr::LOCALHOST, p))) || answers(SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, p))))
}

/// Can this Mac bind the IPv6 loopback address at all?
pub fn ipv6_loopback() -> bool {
    std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_ok()
}

/// Free on both loopback families: a browser asked for `localhost` tries
/// ::1 as well as 127.0.0.1, so a program holding either would answer some
/// of its requests (spec v0.4, Finding "localhost has two addresses").
pub fn port_free_dual(p: u16, ipv6: bool) -> bool {
    if !port_free(p) {
        return false;
    }
    !ipv6 || std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, p)).is_ok()
}

/// A decade for the portal and `games` game origins: the plan's 40700 if
/// all its ports are free, else the first free decade from 41700.
pub fn allocate_games(games: usize, taken: &[u16], ipv6: bool) -> Option<u16> {
    let ok = |base: u16| (0..=games as u16).all(|i| !taken.contains(&(base + i)) && port_free_dual(base + i, ipv6));
    if ok(GAMES_BASE) {
        return Some(GAMES_BASE);
    }
    (GAMES_FALLBACK_START..=FALLBACK_END).step_by(10).find(|b| ok(*b))
}

pub fn block_free(b: Block, taken: &[u16]) -> bool {
    b.all().iter().all(|p| !taken.contains(p) && port_free(*p))
}

/// The plan's base if usable, else the first usable base in the fallback
/// range. `taken` holds ports already allocated to other nodes of this shard
/// (which may not be listening yet).
pub fn allocate(preferred: u16, taken: &[u16]) -> Option<Block> {
    let want = Block(preferred);
    if block_free(want, taken) {
        return Some(want);
    }
    (FALLBACK_START..=FALLBACK_END).step_by(10).map(Block).find(|b| block_free(*b, taken))
}

pub fn allocate_single(preferred: u16, taken: &[u16]) -> Option<u16> {
    if !taken.contains(&preferred) && port_free(preferred) {
        return Some(preferred);
    }
    (FALLBACK_START + 6..=FALLBACK_END).step_by(10).find(|p| !taken.contains(p) && port_free(*p))
}

/// Who holds a port, for the "port in use" message (via `lsof`).
pub fn holder(p: u16) -> Option<String> {
    let out = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{p}"), "-sTCP:LISTEN", "-Fcp"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let pid = s.lines().find_map(|l| l.strip_prefix('p'))?;
    let cmd = s.lines().find_map(|l| l.strip_prefix('c')).unwrap_or("?");
    Some(format!("{cmd} (pid {pid})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_matches_spec() {
        let bases: Vec<u16> = (1..=10).map(validator_base).collect();
        assert_eq!(bases, vec![40410, 40420, 40430, 40440, 40460, 40470, 40480, 40490, 40500, 40510]);
        assert!(!bases.contains(&OBSERVER_BASE));
        assert_eq!(Block(40410).http(), 40413);
    }

    #[test]
    fn a_wildcard_listener_makes_a_port_taken() {
        // Another program listening on all addresses (as Docker does).
        let l = TcpListener::bind("0.0.0.0:0").unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(!port_free(p));
        drop(l);
        assert!(port_free(p));
    }

    #[test]
    fn a_games_decade_avoids_taken_ports_on_either_family() {
        let v6 = ipv6_loopback();
        let base = allocate_games(2, &[], v6).unwrap();
        assert_eq!(base % 10, 0);
        let again = allocate_games(2, &[base, base + 1, base + 2], v6).unwrap();
        assert_ne!(again, base);
        if v6 {
            // A decoy on ::1 alone makes the port taken.
            let l = std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).unwrap();
            let p = l.local_addr().unwrap().port();
            assert!(!port_free_dual(p, true));
        }
    }

    #[test]
    fn a_taken_block_falls_back() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let b = allocate(p, &[]).unwrap();
        assert_ne!(b.0, p);
        let again = allocate(b.0, &b.all()).unwrap();
        assert_ne!(again, b, "ports already given to this shard are not reused");
    }
}
