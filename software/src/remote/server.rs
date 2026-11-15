//! The remote's network side: a WebSocket server on the local network,
//! advertised over Bonjour/mDNS, with PIN pairing. Runs on its own threads;
//! commands reach the app through a channel and state comes back through
//! `Shared::state`.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use tungstenite::{Message, WebSocket};

use super::protocol::{ClientMsg, SERVICE_TYPE, ServerInfo, ServerMsg};
use crate::store::Store;

/// First port tried; the next few are used if it's taken.
pub const PORT: u16 = 47800;
/// Wrong codes before the code changes.
const MAX_PIN_FAILURES: u32 = 5;
const DEVICES_KEY: &str = "remote.devices";

/// A phone that entered the code.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PairedDevice {
    pub name: String,
    pub token: String,
    /// Unix seconds.
    pub last_seen: i64,
}

/// State shared between the app and the server threads.
pub struct Shared {
    pub info: ServerInfo,
    /// Latest state as a ready-to-send `ServerMsg::State` frame, with a sequence number.
    pub state: ArcSwap<(u64, String)>,
    pub pairing: Mutex<Pairing>,
    /// Names of the paired phones connected now.
    pub clients: Mutex<Vec<String>>,
    store: Arc<Store>,
}

pub struct Pairing {
    pub pin: String,
    failures: u32,
    /// A phone asking to pair (name, when), shown on the computer.
    pub request: Option<(String, Instant)>,
    pub devices: Vec<PairedDevice>,
}

impl Shared {
    pub fn new(store: Arc<Store>, info: ServerInfo) -> Arc<Self> {
        let devices = store.setting(DEVICES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Arc::new(Self {
            info,
            state: ArcSwap::from_pointee((0, String::new())),
            pairing: Mutex::new(Pairing { pin: new_pin(), failures: 0, request: None, devices }),
            clients: Mutex::new(Vec::new()),
            store,
        })
    }

    pub fn save_devices(&self, devices: &[PairedDevice]) {
        self.store.set_setting(DEVICES_KEY, &serde_json::to_string(devices).unwrap_or_default());
    }

    pub fn forget(&self, token: &str) {
        let mut p = self.pairing.lock().unwrap();
        p.devices.retain(|d| d.token != token);
        self.save_devices(&p.devices);
    }

    fn known(&self, token: &str) -> Option<String> {
        let mut p = self.pairing.lock().unwrap();
        let d = p.devices.iter_mut().find(|d| d.token == token)?;
        d.last_seen = crate::store::now();
        let name = d.name.clone();
        let devices = p.devices.clone();
        drop(p);
        self.save_devices(&devices);
        Some(name)
    }

    /// Check a code. `Ok(token)` pairs the phone.
    pub(super) fn pair(&self, device: &str, pin: &str) -> Result<String, (String, bool)> {
        let mut p = self.pairing.lock().unwrap();
        if pin.trim() == p.pin {
            let token = random_hex(16);
            let name = if device.trim().is_empty() { "Phone".to_owned() } else { device.trim().to_owned() };
            p.devices.push(PairedDevice { name, token: token.clone(), last_seen: crate::store::now() });
            p.request = None;
            p.failures = 0;
            // A new code for the next phone.
            p.pin = new_pin();
            let devices = p.devices.clone();
            drop(p);
            self.save_devices(&devices);
            return Ok(token);
        }
        p.failures += 1;
        if p.failures >= MAX_PIN_FAILURES {
            p.failures = 0;
            p.pin = new_pin();
            return Err(("Too many wrong codes. A new code is showing on the computer.".into(), true));
        }
        Err(("That code isn't right. Check the code on the computer.".into(), false))
    }
}

/// A running server; dropping it stops it.
pub struct Server {
    pub port: u16,
    stop: Arc<AtomicBool>,
    mdns: Option<(mdns_sd::ServiceDaemon, String)>,
}

impl Server {
    pub fn start(shared: Arc<Shared>, tx: Sender<ClientMsg>, ctx: egui::Context) -> Result<Self, String> {
        Self::start_on(shared, tx, ctx, "0.0.0.0", true)
    }

    /// `bind`: the address to listen on; `discoverable`: advertise over mDNS.
    pub fn start_on(shared: Arc<Shared>, tx: Sender<ClientMsg>, ctx: egui::Context, bind: &str, discoverable: bool) -> Result<Self, String> {
        let listener = (PORT..PORT + 10)
            .find_map(|p| TcpListener::bind((bind, p)).ok())
            .ok_or("Couldn't open a network port for the remote")?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (stop, shared) = (stop.clone(), shared.clone());
            std::thread::Builder::new()
                .name("remote-listen".into())
                .spawn(move || accept_loop(listener, shared, tx, ctx, stop))
                .map_err(|e| e.to_string())?;
        }
        // Discovery is a convenience: the phone can still type the address.
        let mdns = discoverable.then(|| advertise(&shared.info, port).map_err(|e| eprintln!("remote: mDNS unavailable: {e}")).ok()).flatten();
        Ok(Self { port, stop, mdns })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some((daemon, fullname)) = self.mdns.take() {
            if let Ok(rx) = daemon.unregister(&fullname) {
                let _ = rx.recv_timeout(Duration::from_millis(500));
            }
            let _ = daemon.shutdown();
        }
    }
}

fn advertise(info: &ServerInfo, port: u16) -> Result<(mdns_sd::ServiceDaemon, String), String> {
    let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    // Advertise LAN IPv4 only: a phone can't use this computer's loopback or
    // interface-scoped (fe80::…%lo0) IPv6 addresses.
    let _ = daemon.disable_interface(mdns_sd::IfKind::LoopbackV4);
    let _ = daemon.disable_interface(mdns_sd::IfKind::LoopbackV6);
    let _ = daemon.disable_interface(mdns_sd::IfKind::IPv6);
    let host = format!("{}.local.", info.host.replace(' ', "-"));
    let ips: Vec<IpAddr> = local_addresses().into_iter().map(|(_, ip)| ip).collect();
    let props = [("name", info.name.as_str()), ("host", info.host.as_str()), ("id", info.id.as_str()), ("ver", info.version.as_str())];
    // The instance name must be unique on the network: include the computer's name.
    let instance = format!("StageWeave on {}", info.host);
    let service = mdns_sd::ServiceInfo::new(SERVICE_TYPE, &instance, &host, &ips[..], port, &props[..])
        .map_err(|e| e.to_string())?
        .enable_addr_auto();
    let fullname = service.get_fullname().to_owned();
    daemon.register(service).map_err(|e| e.to_string())?;
    Ok((daemon, fullname))
}

/// Phones on the same network only: private, link-local and loopback addresses.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// This computer's addresses on the local network (interface, IPv4).
pub fn local_addresses() -> Vec<(String, IpAddr)> {
    let mut v: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.ip().is_ipv4() && is_local(i.ip()))
        // Skip virtual adapters (VPN tunnels, Docker bridges) where possible.
        .filter(|i| !["utun", "bridge", "docker", "vboxnet", "vmnet", "awdl", "llw"].iter().any(|p| i.name.starts_with(p)))
        .map(|i| (i.name.clone(), i.ip()))
        .collect();
    // Link-local (169.254) last: it usually means no DHCP on that interface.
    v.sort_by_key(|(name, ip)| (matches!(ip, IpAddr::V4(v4) if v4.is_link_local()), name.clone()));
    v.dedup_by(|a, b| a.1 == b.1);
    v
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, tx: Sender<ClientMsg>, ctx: egui::Context, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if !is_local(peer.ip()) {
                    continue; // dropped: not on this network
                }
                let (shared, tx, ctx, stop) = (shared.clone(), tx.clone(), ctx.clone(), stop.clone());
                let _ = std::thread::Builder::new().name("remote-conn".into()).spawn(move || serve(stream, peer, shared, tx, ctx, stop));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn send(ws: &mut WebSocket<TcpStream>, msg: &ServerMsg) -> bool {
    ws.send(Message::text(serde_json::to_string(msg).unwrap_or_default())).is_ok()
}

fn serve(stream: TcpStream, _peer: SocketAddr, shared: Arc<Shared>, tx: Sender<ClientMsg>, ctx: egui::Context, stop: Arc<AtomicBool>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    // Short reads so state changes go out promptly.
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(50)));
    let mut device: Option<String> = None;
    let mut token: Option<String> = None;
    let mut sent_seq = 0;
    let mut ticks = 0u32;
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ws.close(None);
            let _ = ws.flush();
            break;
        }
        let text = match ws.read() {
            Ok(Message::Text(t)) => Some(t.to_string()),
            Ok(Message::Close(_)) => break,
            Ok(_) => None,
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
            Err(_) => break,
        };
        if let Some(text) = text {
            let msg: ClientMsg = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    if !send(&mut ws, &ServerMsg::Error { message: format!("Bad message: {e}") }) {
                        break;
                    }
                    continue;
                }
            };
            match msg {
                ClientMsg::Hello { device: name, token: t, .. } => {
                    let known = t.as_deref().and_then(|t| shared.known(t));
                    if let Some(name) = known {
                        if device.is_none() {
                            shared.clients.lock().unwrap().push(name.clone());
                        }
                        device = Some(name);
                        token = t;
                        sent_seq = 0; // send the state right away
                        let ok = send(&mut ws, &ServerMsg::Welcome { server: shared.info.clone() });
                        ctx.request_repaint();
                        if !ok {
                            break;
                        }
                    } else {
                        shared.pairing.lock().unwrap().request = Some((name, Instant::now()));
                        ctx.request_repaint();
                        if !send(&mut ws, &ServerMsg::PairRequired { server: shared.info.clone() }) {
                            break;
                        }
                    }
                }
                ClientMsg::Pair { device: name, pin } => match shared.pair(&name, &pin) {
                    Ok(t) => {
                        let name = shared.known(&t).unwrap_or(name);
                        if device.is_none() {
                            shared.clients.lock().unwrap().push(name.clone());
                        }
                        device = Some(name);
                        token = Some(t.clone());
                        sent_seq = 0;
                        ctx.request_repaint();
                        if !send(&mut ws, &ServerMsg::Paired { token: t, server: shared.info.clone() }) {
                            break;
                        }
                    }
                    Err((message, drop_connection)) => {
                        ctx.request_repaint();
                        if !send(&mut ws, &ServerMsg::PairFailed { message }) || drop_connection {
                            break;
                        }
                    }
                },
                ClientMsg::Ping => {
                    if !send(&mut ws, &ServerMsg::Pong) {
                        break;
                    }
                }
                cmd if device.is_some() => {
                    let _ = tx.send(cmd);
                    ctx.request_repaint();
                }
                _ => {
                    if !send(&mut ws, &ServerMsg::Error { message: "Pair this phone first".into() }) {
                        break;
                    }
                }
            }
        }
        if device.is_some() {
            // Removed on the computer: disconnect.
            ticks += 1;
            if ticks.is_multiple_of(20) {
                let still = token.as_deref().is_some_and(|t| shared.pairing.lock().unwrap().devices.iter().any(|d| d.token == t));
                if !still {
                    let _ = send(&mut ws, &ServerMsg::Error { message: "This phone was removed on the computer. Pair it again.".into() });
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    break;
                }
            }
            let state = shared.state.load();
            if state.0 != sent_seq && !state.1.is_empty() {
                sent_seq = state.0;
                if ws.send(Message::text(state.1.clone())).is_err() {
                    break;
                }
            }
        }
    }
    if let Some(name) = device {
        let mut c = shared.clients.lock().unwrap();
        if let Some(i) = c.iter().position(|n| *n == name) {
            c.remove(i);
        }
        ctx.request_repaint();
    }
}

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    if getrandom::fill(&mut b).is_err() {
        // Extremely unlikely; fall back to the clock.
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        b.iter_mut().enumerate().for_each(|(i, x)| *x = (n >> ((i % 16) * 8)) as u8);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn new_pin() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    format!("{:04}", u32::from_le_bytes(b) % 10_000)
}
