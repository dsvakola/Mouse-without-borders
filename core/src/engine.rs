//! Runs a master or a secondary PC: connections, input sharing, clipboard and files.
//! Windows only. Everything here is driven by the tested core modules.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::channel::SecureReceiver;
use crate::discovery::{
    broadcast_targets, listen_socket, local_ipv4s, probe_bytes, recv_packet, send_to_all, Beacon,
    Packet, CONNECT_PORT, DISCOVERY_PORT,
};
use crate::files::FileReceiver;
use crate::handshake::{self, PeerInfo};
use crate::held::{Held, KeyId, Route};
use crate::layout::{layout_from_cells, Layout, Placed, Screen, GRID_COLS, GRID_ROWS};
use crate::limiter::AttemptLimiter;
use crate::platform::{self, Captured};
use crate::protocol::{Button, Message, FILE_CHUNK, PROTOCOL_VERSION};
use crate::router::{Action, Router};
use crate::store::{self, Cell, Settings};
use crate::{AppError, Result};

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// Switches the user can change while running.
pub struct Options {
    pub clipboard: AtomicBool,
    pub files: AtomicBool,
}

impl Options {
    pub fn from_settings(s: &Settings) -> Arc<Options> {
        Arc::new(Options {
            clipboard: AtomicBool::new(s.share_clipboard),
            files: AtomicBool::new(s.transfer_files),
        })
    }
    pub fn set(&self, clipboard: bool, files: bool) {
        self.clipboard.store(clipboard, Ordering::Relaxed);
        self.files.store(files, Ordering::Relaxed);
    }
}

/// Files larger than this are not shared automatically.
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// Copied text longer than this is not shared (it must fit in one message).
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const PING_EVERY: Duration = Duration::from_secs(3);
const DEAD_AFTER: Duration = Duration::from_secs(10);
/// A held mouse button whose release never arrived is forgotten after this.
const STALE_BUTTON: Duration = Duration::from_secs(60);

/// Load saved settings or create them for the first run.
pub fn load_settings(dir: &std::path::Path, machine_name: &str) -> Result<Settings> {
    Ok(match store::load(dir)? {
        Some(s) => s,
        None => {
            let s = Settings::new(machine_name);
            store::save(dir, &s)?;
            s
        }
    })
}

/// After a restart the old copy may still hold its ports for a moment.
fn retry<T, E>(mut f: impl FnMut() -> std::result::Result<T, E>) -> std::result::Result<T, E> {
    let mut last = f();
    for _ in 0..25 {
        if last.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(200));
        last = f();
    }
    last
}

fn me(s: &Settings) -> PeerInfo {
    PeerInfo {
        id: s.machine_id.clone(),
        name: s.machine_name.clone(),
    }
}

/// Name Windows uses for this PC on the network.
fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into())
}

/// The discovery socket: the usual port if possible, otherwise any port
/// (searching still works, because answers come back to whatever port asked).
fn discovery_socket(log: &Log) -> Result<UdpSocket> {
    match retry(|| listen_socket(DISCOVERY_PORT)) {
        Ok(s) => Ok(s),
        Err(e) => {
            log(format!(
                "Port {DISCOVERY_PORT} is in use by another program ({e}); searching continues on another port."
            ));
            listen_socket(0)
        }
    }
}

/// The outgoing side of one connection. Mouse and keyboard go through `tx`
/// and are always sent first; file data waits in `bulk`, so a large file
/// never makes the mouse feel slow.
#[derive(Clone)]
struct Link {
    tx: SyncSender<Message>,
    bulk: SyncSender<Message>,
}

impl Link {
    fn open(sender: crate::channel::SecureSender<TcpStream>) -> Link {
        let (tx, rx) = sync_channel::<Message>(2048);
        let (bulk, bulk_rx) = sync_channel::<Message>(8);
        spawn_writer(sender, rx, bulk_rx);
        Link { tx, bulk }
    }

    /// Queue an input message; never waits.
    fn queue(&self, m: Message) {
        // Full means the other PC is stuck; the heartbeat will drop it.
        let _ = self.tx.try_send(m);
    }

    /// Queue file data; waits while the network is busy.
    fn send_bulk(&self, m: Message) -> Result<()> {
        self.bulk
            .send(m)
            .map_err(|_| AppError::Io("connection closed".into()))
    }
}

/// Writer thread: input first, then file data, plus a heartbeat when idle.
fn spawn_writer(
    mut sender: crate::channel::SecureSender<TcpStream>,
    rx: Receiver<Message>,
    bulk: Receiver<Message>,
) {
    thread::spawn(move || {
        let mut last = Instant::now();
        loop {
            let next = match rx.try_recv() {
                Ok(m) => Some(m),
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => match bulk.try_recv() {
                    Ok(m) => Some(m),
                    Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => match rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                },
            };
            let msg = match next {
                Some(m) => m,
                None if last.elapsed() >= PING_EVERY => Message::Ping,
                None => continue,
            };
            if sender.send(&msg).is_err() {
                return;
            }
            last = Instant::now();
        }
    });
}

fn prepare(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(DEAD_AFTER))?;
    // A PC that stopped answering must not block the sender forever.
    stream.set_write_timeout(Some(DEAD_AFTER))?;
    Ok(())
}

fn is_file_message(m: &Message) -> bool {
    matches!(
        m,
        Message::FileOffer { .. } | Message::FileChunk { .. } | Message::FileEnd { .. }
    )
}

// ------------------------------------------------------------------ sharing

/// Handles clipboard text and files arriving from the network.
struct Incoming {
    receiver: FileReceiver,
    batch: Vec<PathBuf>,
    /// Clipboard sequence number after we last set it ourselves.
    ignore_seq: Arc<Mutex<u32>>,
    options: Arc<Options>,
}

impl Incoming {
    fn new(ignore_seq: Arc<Mutex<u32>>, options: Arc<Options>) -> Result<Incoming> {
        let dir = platform::shared_files_dir()?;
        Ok(Incoming {
            receiver: FileReceiver::new(dir),
            batch: vec![],
            ignore_seq,
            options,
        })
    }

    /// Returns true if the message was a sharing message (and so handled).
    fn handle(&mut self, msg: &Message, log: &Log) -> bool {
        let is_file = is_file_message(msg);
        if (is_file && !self.options.files.load(Ordering::Relaxed))
            || (matches!(msg, Message::ClipboardText(_))
                && !self.options.clipboard.load(Ordering::Relaxed))
        {
            return true; // switched off: drop it quietly
        }
        match msg {
            Message::ClipboardText(t) => {
                if platform::set_clipboard_text(t).is_ok() {
                    *self.ignore_seq.lock().unwrap() = platform::clipboard_sequence();
                }
            }
            Message::FileOffer {
                id,
                name,
                size,
                index,
                ..
            } => {
                if *index == 0 {
                    self.batch.clear();
                }
                if let Err(e) = self.receiver.begin(*id, name, *size) {
                    log(format!("Could not receive {name}: {e}"));
                }
            }
            Message::FileChunk { id, data } => {
                if let Err(e) = self.receiver.chunk(*id, data) {
                    log(format!("File transfer problem: {e}"));
                }
            }
            Message::FileEnd { id } => match self.receiver.finish(*id) {
                Ok(path) => {
                    log(format!("Received file: {}", path.display()));
                    self.batch.push(path);
                }
                Err(e) => log(format!("File transfer problem: {e}")),
            },
            _ => return false,
        }
        true
    }
}

/// Reads a file and queues it for sending, `index` of `total` in its batch.
fn send_file(link: &Link, path: &PathBuf, id: u64, index: u32, total: u32) -> Result<()> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    link.send_bulk(Message::FileOffer {
        id,
        name,
        size,
        index,
        total,
    })?;
    let mut buf = vec![0u8; FILE_CHUNK];
    let mut left = size;
    while left > 0 {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // Never send more than announced, even if the file is still growing.
        let n = n.min(left.min(usize::MAX as u64) as usize);
        left -= n as u64;
        link.send_bulk(Message::FileChunk {
            id,
            data: buf[..n].to_vec(),
        })?;
    }
    link.send_bulk(Message::FileEnd { id })
}

/// File ids must differ between PCs, because the master passes files on.
fn file_id_base(machine_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    machine_id.hash(&mut h);
    h.finish() << 32
}

/// Watches the local clipboard and sends changes to `targets()`.
fn spawn_clipboard_watcher(
    machine_id: &str,
    options: Arc<Options>,
    ignore_seq: Arc<Mutex<u32>>,
    targets: impl Fn() -> Vec<Link> + Send + 'static,
    log: Log,
) {
    let id_base = file_id_base(machine_id);
    thread::spawn(move || {
        let mut last = platform::clipboard_sequence();
        let mut counter = 0u64;
        loop {
            thread::sleep(Duration::from_millis(400));
            let now = platform::clipboard_sequence();
            if now == last {
                continue;
            }
            last = now;
            if *ignore_seq.lock().unwrap() == now {
                continue; // we set it ourselves
            }
            let peers = targets();
            if peers.is_empty() {
                continue;
            }
            let files = if options.files.load(Ordering::Relaxed) {
                platform::get_clipboard_files()
            } else {
                vec![]
            };
            if !files.is_empty() {
                let files: Vec<PathBuf> = files
                    .into_iter()
                    .filter(|p| {
                        p.metadata()
                            .map(|m| m.len() <= MAX_FILE_BYTES)
                            .unwrap_or(false)
                    })
                    .collect();
                let total = files.len() as u32;
                for (i, f) in files.iter().enumerate() {
                    counter = (counter + 1) & 0xFFFF_FFFF;
                    for link in &peers {
                        if let Err(e) = send_file(link, f, id_base | counter, i as u32, total) {
                            log(format!("Could not send {}: {e}", f.display()));
                        }
                    }
                }
            } else if !options.clipboard.load(Ordering::Relaxed) {
                continue;
            } else if let Some(t) = platform::get_clipboard_text() {
                if t.len() > MAX_TEXT_BYTES {
                    log("The copied text is too long to share (over 1 MB).".to_string());
                    continue;
                }
                for link in &peers {
                    link.queue(Message::ClipboardText(t.clone()));
                }
            }
        }
    });
}

// ------------------------------------------------------------------- master

/// Gives every connection its own number, so a late clean-up of an old
/// connection can never remove a newer connection of the same PC.
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

struct Peer {
    link: Link,
    name: String,
    conn: u64,
}

struct MasterState {
    router: Router,
    /// Connected computers with screen size; this PC first.
    sizes: Vec<(String, i32, i32)>,
    /// Grid cell of every computer we know (including ones that are offline).
    cells: HashMap<String, (u8, u8)>,
    peers: HashMap<String, Peer>,
    local_id: String,
    local_size: (i32, i32),
    grabbed: bool,
    center: (i32, i32),
    held: Held,
    /// Ctrl / Alt as seen by our own hook (Windows does not report keys we hide).
    ctrl: bool,
    alt: bool,
    last_button: Instant,
}

impl MasterState {
    fn send_active(&self, m: Message) {
        if let Some(p) = self.peers.get(self.router.active()) {
            p.link.queue(m);
        }
    }

    fn apply(&mut self, actions: Vec<Action>, log: &Log) {
        for a in actions {
            match a {
                Action::Leave { id } if id == self.local_id => {
                    self.grabbed = true;
                    // From here on Ctrl and Alt are followed by our own hook.
                    self.ctrl = false;
                    self.alt = false;
                    platform::set_cursor_pos(self.center.0, self.center.1);
                }
                Action::Leave { id } => {
                    // Release there whatever is still held there.
                    let (keys, buttons) = self.held.release_remote();
                    if let Some(p) = self.peers.get(&id) {
                        for (scancode, extended) in keys {
                            p.link.queue(Message::Key {
                                scancode,
                                extended,
                                down: false,
                            });
                        }
                        for button in buttons {
                            p.link.queue(Message::MouseButton {
                                button,
                                down: false,
                            });
                        }
                        p.link.queue(Message::Leave);
                    }
                }
                Action::Enter { id, x, y } if id == self.local_id => {
                    // Keep a little away from the edge so we do not bounce straight back.
                    let (w, h) = self.local_size;
                    let (x, y) = (x.clamp(2, (w - 3).max(2)), y.clamp(2, (h - 3).max(2)));
                    self.grabbed = false;
                    platform::set_cursor_pos(x, y);
                    self.router.sync_local(x, y);
                }
                Action::Enter { id, x, y } => {
                    if let Some(p) = self.peers.get(&id) {
                        log(format!("Mouse and keyboard now control {}", p.name));
                        p.link.queue(Message::Enter { x, y });
                    }
                }
                Action::Forward { .. } => {
                    // Send where the cursor should be, not how far it moved: the
                    // other PC's pointer speed settings would otherwise make the
                    // real cursor drift away from what the router tracks.
                    let (x, y) = self.router.position();
                    self.send_active(Message::MouseAbs { x, y });
                }
            }
        }
    }

    /// Recompute screen positions from the arrangement grid.
    /// A computer with no cell, or whose cell is taken, gets the first free one.
    fn rebuild_layout(&mut self) -> Result<()> {
        let mut taken: Vec<(u8, u8)> = Vec::new();
        for (id, _, _) in &self.sizes {
            let ok = self.cells.get(id).is_some_and(|c| !taken.contains(c));
            if !ok {
                let free = (0..GRID_ROWS as u8)
                    .flat_map(|r| (0..GRID_COLS as u8).map(move |c| (c, r)))
                    .find(|c| !taken.contains(c))
                    .ok_or_else(|| AppError::InvalidLayout("no free place".into()))?;
                self.cells.insert(id.clone(), free);
            }
            taken.push(self.cells[id]);
        }
        let placed: Vec<Placed> = self
            .sizes
            .iter()
            .map(|(id, w, h)| {
                let (col, row) = self.cells[id];
                Placed {
                    id: id.clone(),
                    width: *w,
                    height: *h,
                    col,
                    row,
                }
            })
            .collect();
        self.router.set_layout(layout_from_cells(&placed)?);
        Ok(())
    }

    fn cell_list(&self) -> Vec<Cell> {
        self.cells
            .iter()
            .map(|(id, (col, row))| Cell {
                id: id.clone(),
                col: *col,
                row: *row,
            })
            .collect()
    }

    /// Hotkey Ctrl+Alt+F1..F3: F1 is this PC, then the others left to right.
    fn nth_machine(&self, n: usize) -> Option<String> {
        if n == 0 {
            return Some(self.local_id.clone());
        }
        let mut others: Vec<&(String, i32, i32)> =
            self.sizes.iter().filter(|s| s.0 != self.local_id).collect();
        others.sort_by_key(|s| self.cells.get(&s.0).map(|c| (c.0, c.1)));
        others.get(n - 1).map(|s| s.0.clone())
    }
}

/// -1, 0 or 1: is `v` at the low edge, inside, or at the high edge of 0..size.
fn edge(v: i32, size: i32) -> i32 {
    if v >= size - 1 {
        1
    } else if v <= 0 {
        -1
    } else {
        0
    }
}

/// Runs inside the Windows input hook: must be quick and must never wait for
/// the disk or the network (Windows removes hooks that answer slowly).
fn hook_handler(shared: Arc<MasterShared>, log: Log) -> impl Fn(Captured) -> bool + Send + Sync {
    move |ev| {
        let mut guard = shared.state.lock().unwrap();
        let st = &mut *guard;
        if let Captured::Key { scancode, down, .. } = ev {
            match scancode {
                0x1D => st.ctrl = down,
                0x38 => st.alt = down,
                _ => {}
            }
        }
        match ev {
            Captured::Key { scancode, down, .. }
                if (0x3B..=0x3D).contains(&scancode)
                    && ((st.grabbed && st.ctrl && st.alt) || platform::ctrl_alt_down()) =>
            {
                if down {
                    if let Some(id) = st.nth_machine((scancode - 0x3B) as usize) {
                        let acts = st.router.jump_to(&id);
                        st.apply(acts, &log);
                    }
                }
                true
            }
            Captured::MouseMove { x, y } => {
                if st.held.remote_button_held() && st.last_button.elapsed() > STALE_BUTTON {
                    st.held.clear_remote_buttons();
                }
                if !st.grabbed {
                    st.router.sync_local(x, y);
                    let (w, h) = st.local_size;
                    let (dx, dy) = (edge(x, w), edge(y, h));
                    // No switching in the middle of a drag.
                    if (dx != 0 || dy != 0) && !platform::mouse_button_down() {
                        let acts = st.router.feed_delta(dx, dy);
                        st.apply(acts, &log);
                    }
                    // If this move just took the cursor to another PC, swallow it:
                    // letting it through would put the real cursor back at the
                    // edge after it was parked in the centre, and the next move
                    // would then count as a jump of half a screen on the other PC.
                    st.grabbed
                } else {
                    let (dx, dy) = (x - st.center.0, y - st.center.1);
                    if dx != 0 || dy != 0 {
                        let allow = !st.held.remote_button_held();
                        let acts = st.router.feed(dx, dy, allow);
                        st.apply(acts, &log);
                        if st.grabbed {
                            platform::set_cursor_pos(st.center.0, st.center.1);
                        }
                    }
                    true
                }
            }
            Captured::MouseButton { button, down } => {
                st.last_button = Instant::now();
                match st.held.button(button, down, st.grabbed) {
                    Route::Local => false,
                    Route::Remote => {
                        st.send_active(Message::MouseButton { button, down });
                        true
                    }
                }
            }
            Captured::Key {
                scancode,
                extended,
                down,
            } => match st.held.key((scancode, extended), down, st.grabbed) {
                Route::Local => false,
                Route::Remote => {
                    st.send_active(Message::Key {
                        scancode,
                        extended,
                        down,
                    });
                    true
                }
            },
            Captured::Wheel { dx, dy } => {
                if st.grabbed {
                    st.send_active(Message::Wheel { dx, dy });
                }
                st.grabbed
            }
        }
    }
}

/// What the app can see and change while the master runs.
#[derive(Debug, Clone)]
pub struct MachineView {
    pub id: String,
    pub name: String,
    pub is_local: bool,
    pub connected: bool,
    pub col: u8,
    pub row: u8,
}

struct MasterShared {
    state: Mutex<MasterState>,
    limiter: Mutex<AttemptLimiter>,
    settings: Mutex<Settings>,
    dir: PathBuf,
    ignore_seq: Arc<Mutex<u32>>,
    options: Arc<Options>,
    log: Log,
    me: PeerInfo,
    /// Another PC that also says it is the master, and when it was last heard.
    other_master: Mutex<Option<(String, Instant)>>,
}

#[derive(Clone)]
pub struct MasterHandle {
    shared: Arc<MasterShared>,
}

impl MasterHandle {
    /// This PC, every connected secondary, and remembered ones that are offline.
    pub fn machines(&self) -> Vec<MachineView> {
        let st = self.shared.state.lock().unwrap();
        let set = self.shared.settings.lock().unwrap();
        let mut out = vec![];
        for (id, _, _) in &st.sizes {
            let local = *id == st.local_id;
            let name = if local {
                set.machine_name.clone()
            } else {
                st.peers.get(id).map(|p| p.name.clone()).unwrap_or_default()
            };
            let (col, row) = st.cells.get(id).copied().unwrap_or((0, 0));
            out.push(MachineView {
                id: id.clone(),
                name,
                is_local: local,
                connected: true,
                col,
                row,
            });
        }
        let mut taken: Vec<(u8, u8)> = out.iter().map(|m| (m.col, m.row)).collect();
        for p in &set.peers {
            if out.iter().any(|m| m.id == p.id) {
                continue;
            }
            // Offline: keep its saved cell if free, otherwise first free (display only).
            let saved = st.cells.get(&p.id).copied();
            let cell = saved.filter(|c| !taken.contains(c)).or_else(|| {
                (0..GRID_ROWS as u8)
                    .flat_map(|r| (0..GRID_COLS as u8).map(move |c| (c, r)))
                    .find(|c| !taken.contains(c))
            });
            if let Some((col, row)) = cell {
                taken.push((col, row));
                out.push(MachineView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    is_local: false,
                    connected: false,
                    col,
                    row,
                });
            }
        }
        out
    }

    /// Save the arrangement the user made and apply it to connected computers.
    pub fn set_cells(&self, cells: Vec<Cell>, two_row: bool) -> Result<()> {
        let list = {
            let mut st = self.shared.state.lock().unwrap();
            let old = st.cells.clone();
            for c in &cells {
                st.cells.insert(c.id.clone(), (c.col, c.row));
            }
            if let Err(e) = st.rebuild_layout() {
                st.cells = old;
                return Err(e);
            }
            st.cell_list()
        };
        // The disk is touched only after the input state is unlocked.
        let mut set = self.shared.settings.lock().unwrap();
        set.cells = list;
        set.two_row = two_row;
        store::save_learnt(&self.shared.dir, &set, true)
    }

    pub fn options(&self) -> Arc<Options> {
        self.shared.options.clone()
    }

    /// Something the user should fix, if anything.
    pub fn warning(&self) -> Option<String> {
        let other = self.shared.other_master.lock().unwrap();
        match &*other {
            Some((name, at)) if at.elapsed() < Duration::from_secs(8) => Some(format!(
                "\"{name}\" is also set as Master. Only one PC can be the master: on the other PCs choose \"Secondary\" in Machine Setup and press Apply."
            )),
            _ => None,
        }
    }
}

/// Start the master in the background (listener, discovery, input capture).
pub fn start_master(settings_dir: PathBuf, settings: Settings, log: Log) -> Result<MasterHandle> {
    platform::init_dpi();
    store::save(&settings_dir, &settings)?;
    let size = platform::screen_size();
    let local_id = settings.machine_id.clone();
    let first = Layout::new(vec![Screen::new(&local_id, 0, 0, size.0, size.1)])?;
    let router =
        Router::new(first, &local_id).ok_or_else(|| AppError::InvalidLayout("this PC".into()))?;
    let cells: HashMap<String, (u8, u8)> = settings
        .cells
        .iter()
        .map(|c| (c.id.clone(), (c.col, c.row)))
        .collect();
    let mut st = MasterState {
        router,
        sizes: vec![(local_id.clone(), size.0, size.1)],
        cells,
        peers: HashMap::new(),
        local_id: local_id.clone(),
        local_size: size,
        grabbed: false,
        center: (size.0 / 2, size.1 / 2),
        held: Held::default(),
        ctrl: false,
        alt: false,
        last_button: Instant::now(),
    };
    st.rebuild_layout()?;
    let options = Options::from_settings(&settings);
    let shared = Arc::new(MasterShared {
        state: Mutex::new(st),
        limiter: Mutex::new(AttemptLimiter::new()),
        me: me(&settings),
        settings: Mutex::new(settings.clone()),
        dir: settings_dir,
        ignore_seq: Arc::new(Mutex::new(0u32)),
        options: options.clone(),
        log: log.clone(),
        other_master: Mutex::new(None),
    });

    // Take the connection port first: if it is busy nothing else should start.
    let listener = retry(|| TcpListener::bind(("0.0.0.0", CONNECT_PORT))).map_err(|e| {
        AppError::Io(format!(
            "cannot listen on port {CONNECT_PORT} (is another copy running?): {e}"
        ))
    })?;

    platform::start_capture(hook_handler(shared.clone(), log.clone()))?;

    // Tell the network we are here, and answer PCs that are searching.
    {
        let sock = discovery_socket(&log)?;
        let bytes = Beacon::new(&settings.machine_id, &settings.machine_name, true).to_bytes()?;
        let sh = shared.clone();
        thread::spawn(move || master_discovery(&sock, &bytes, &sh));
    }

    // Notice a changed screen resolution, and keep this PC awake while its
    // mouse and keyboard are busy on another PC.
    {
        let sh = shared.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(3));
            let size = platform::screen_size();
            let mut st = sh.state.lock().unwrap();
            if st.grabbed {
                platform::keep_awake();
            }
            if size != st.local_size && size.0 > 0 && size.1 > 0 {
                st.local_size = size;
                st.center = (size.0 / 2, size.1 / 2);
                let id = st.local_id.clone();
                if let Some(e) = st.sizes.iter_mut().find(|e| e.0 == id) {
                    *e = (id, size.0, size.1);
                }
                let _ = st.rebuild_layout();
            }
        });
    }

    // Clipboard from this PC goes to every secondary.
    {
        let sh = shared.clone();
        spawn_clipboard_watcher(
            &settings.machine_id,
            options,
            shared.ignore_seq.clone(),
            move || {
                sh.state
                    .lock()
                    .unwrap()
                    .peers
                    .values()
                    .map(|p| p.link.clone())
                    .collect()
            },
            log.clone(),
        );
    }

    log(format!(
        "Master \"{}\" is ready. Hotkeys: Ctrl+Alt+F1 = this PC, F2/F3 = other PCs.",
        settings.machine_name
    ));
    let accept_shared = shared.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else {
                thread::sleep(Duration::from_millis(200));
                continue;
            };
            let sh = accept_shared.clone();
            thread::spawn(move || {
                if let Err(e) = master_connection(stream, &sh) {
                    (sh.log)(format!("Connection ended: {e}"));
                }
            });
        }
    });
    Ok(MasterHandle { shared })
}

fn master_discovery(sock: &UdpSocket, beacon: &[u8], sh: &MasterShared) {
    let host = host_name();
    let mut targets = Vec::new();
    let mut round = 0u32;
    loop {
        // Network adapters come and go (Wi-Fi reconnects, sleep): look again regularly.
        if round.is_multiple_of(5) {
            targets = broadcast_targets(&local_ipv4s(&host), DISCOVERY_PORT);
        }
        round = round.wrapping_add(1);
        send_to_all(sock, beacon, &targets);
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match recv_packet(sock, &sh.me.id, left) {
                // A secondary is searching: answer it directly.
                Ok(Some((Packet::Probe { .. }, from))) => {
                    let _ = sock.send_to(beacon, from);
                }
                Ok(Some((Packet::Beacon(b), _))) if b.is_master => {
                    *sh.other_master.lock().unwrap() = Some((b.name, Instant::now()));
                }
                Ok(_) => {}
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

fn master_connection(stream: TcpStream, sh: &Arc<MasterShared>) -> Result<()> {
    let ip: IpAddr = stream.peer_addr()?.ip();
    if let Err(secs) = sh.limiter.lock().unwrap().check(ip, Instant::now()) {
        return Err(AppError::LockedOut(secs));
    }
    prepare(&stream)?;
    let result = handshake::accept(
        stream,
        &sh.me,
        &|id| sh.settings.lock().unwrap().secret_for(id),
        crate::SHARED_PIN,
    );
    let est = match result {
        Ok(e) => {
            sh.limiter.lock().unwrap().record_success(ip);
            e
        }
        Err(e) => {
            if matches!(e, AppError::WrongPin) {
                sh.limiter
                    .lock()
                    .unwrap()
                    .record_failure(ip, Instant::now());
            }
            return Err(e);
        }
    };
    let log = &sh.log;
    if let Some(secret) = est.new_secret {
        let mut s = sh.settings.lock().unwrap();
        s.save_pairing(&est.peer.id, &est.peer.name, &secret);
        store::save_learnt(&sh.dir, &s, true)?;
        log(format!("{} paired successfully.", est.peer.name));
    }
    let peer = est.peer;
    let (sender, mut receiver) = est.channel.split(|s| s.try_clone())?;

    // First message from a secondary: its screen size.
    let (w, h) = match receiver.recv()? {
        Message::ScreenInfo { width, height } if width > 0 && height > 0 => (width, height),
        _ => return Err(AppError::Protocol("screen size missing".into())),
    };
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let cells = {
        let mut st = sh.state.lock().unwrap();
        if st.sizes.len() >= crate::layout::MAX_MACHINES && !st.sizes.iter().any(|s| s.0 == peer.id)
        {
            return Err(AppError::InvalidLayout(
                "this master already has 3 computers".into(),
            ));
        }
        // The same PC may connect again before its old connection has timed out.
        let before = st.sizes.iter().find(|s| s.0 == peer.id).cloned();
        st.sizes.retain(|s| s.0 != peer.id);
        st.sizes.push((peer.id.clone(), w, h));
        if let Err(e) = st.rebuild_layout() {
            st.sizes.retain(|s| s.0 != peer.id);
            st.sizes.extend(before);
            let _ = st.rebuild_layout();
            return Err(e);
        }
        st.peers.insert(
            peer.id.clone(),
            Peer {
                link: Link::open(sender),
                name: peer.name.clone(),
                conn,
            },
        );
        st.cell_list()
    };
    {
        let mut set = sh.settings.lock().unwrap();
        set.cells = cells;
        let _ = store::save_learnt(&sh.dir, &set, true);
    }
    log(format!("{} connected ({}x{}).", peer.name, w, h));

    let outcome = match Incoming::new(sh.ignore_seq.clone(), sh.options.clone()) {
        Ok(mut incoming) => master_receive(&mut receiver, &mut incoming, sh, &peer, conn),
        Err(e) => Err(e),
    };

    // Connection gone: take the cursor back if this PC had it. A newer
    // connection of the same PC is left alone.
    let mut st = sh.state.lock().unwrap();
    if st.peers.get(&peer.id).is_some_and(|p| p.conn == conn) {
        st.peers.remove(&peer.id);
        let acts = st.router.machine_lost(&peer.id);
        if !acts.is_empty() {
            st.held.release_remote();
        }
        st.apply(acts, log);
        st.sizes.retain(|s| s.0 != peer.id);
        let _ = st.rebuild_layout();
        log(format!("{} disconnected.", peer.name));
    }
    outcome
}

fn master_receive(
    receiver: &mut SecureReceiver<TcpStream>,
    incoming: &mut Incoming,
    sh: &MasterShared,
    peer: &PeerInfo,
    conn: u64,
) -> Result<()> {
    let log = &sh.log;
    loop {
        let msg = receiver.recv()?;
        // A newer connection of this PC has taken over: stop quietly.
        let others: Vec<Link> = {
            let st = sh.state.lock().unwrap();
            if st.peers.get(&peer.id).is_none_or(|p| p.conn != conn) {
                return Ok(());
            }
            st.peers
                .iter()
                .filter(|(id, _)| **id != peer.id)
                .map(|(_, p)| p.link.clone())
                .collect()
        };
        if let Message::ScreenInfo { width, height } = msg {
            // The secondary changed its screen resolution.
            if width > 0 && height > 0 {
                let mut st = sh.state.lock().unwrap();
                if let Some(e) = st.sizes.iter_mut().find(|e| e.0 == peer.id) {
                    *e = (peer.id.clone(), width, height);
                }
                let _ = st.rebuild_layout();
            }
            continue;
        }
        let is_file = is_file_message(&msg);
        let is_text = matches!(msg, Message::ClipboardText(_));
        if !incoming.handle(&msg, log) {
            continue;
        }
        // Pass what a secondary copied on to the other secondaries.
        if is_text && sh.options.clipboard.load(Ordering::Relaxed) {
            for l in &others {
                l.queue(msg.clone());
            }
        } else if is_file && sh.options.files.load(Ordering::Relaxed) {
            for l in &others {
                let _ = l.send_bulk(msg.clone());
            }
        }
        if let Message::FileEnd { .. } = msg {
            finish_batch(incoming, log);
        }
    }
}

/// Once files have arrived, offer them for pasting on this PC too.
fn finish_batch(incoming: &mut Incoming, log: &Log) {
    if incoming.batch.is_empty() {
        return;
    }
    if platform::set_clipboard_files(&incoming.batch).is_ok() {
        *incoming.ignore_seq.lock().unwrap() = platform::clipboard_sequence();
    } else {
        log("Could not place the files on the clipboard.".to_string());
    }
}

// ---------------------------------------------------------------- secondary

struct SecondaryShared {
    connected_to: Mutex<Option<String>>,
    link: Mutex<Option<Link>>,
    /// When the current search for the master began.
    searching_since: Mutex<Option<Instant>>,
    /// A problem found while searching that the user should know about.
    problem: Mutex<Option<String>>,
    settings: Mutex<Settings>,
    dir: PathBuf,
    ignore_seq: Arc<Mutex<u32>>,
    options: Arc<Options>,
    log: Log,
}

#[derive(Clone)]
pub struct SecondaryHandle {
    shared: Arc<SecondaryShared>,
}

impl SecondaryHandle {
    /// Name of the master we are connected to, if any.
    pub fn connected_to(&self) -> Option<String> {
        self.shared.connected_to.lock().unwrap().clone()
    }

    pub fn options(&self) -> Arc<Options> {
        self.shared.options.clone()
    }

    /// Something the user should fix, if anything.
    pub fn warning(&self) -> Option<String> {
        if self.connected_to().is_some() {
            return None;
        }
        if let Some(p) = self.shared.problem.lock().unwrap().clone() {
            return Some(p);
        }
        let since = *self.shared.searching_since.lock().unwrap();
        match since {
            Some(t) if t.elapsed() > Duration::from_secs(20) => Some(
                "The master PC has not been found yet. Check that it is switched on, signed in, set as Master and on the same network. If it still fails, press \"Allow through firewall\" on the master PC."
                    .into(),
            ),
            _ => None,
        }
    }
}

/// Start as a secondary PC in the background. It finds the master by itself
/// (or uses `settings.master_ip`) and reconnects whenever the link drops.
pub fn start_secondary(
    settings_dir: PathBuf,
    settings: Settings,
    log: Log,
) -> Result<SecondaryHandle> {
    platform::init_dpi();
    let options = Options::from_settings(&settings);
    let fixed: Option<SocketAddr> = if settings.master_ip.trim().is_empty() {
        None
    } else {
        let ip = settings.master_ip.trim();
        Some(SocketAddr::new(
            ip.parse::<IpAddr>().map_err(|_| {
                AppError::Io(format!(
                    "\"{ip}\" is not a valid address (example: 192.168.1.10)"
                ))
            })?,
            CONNECT_PORT,
        ))
    };
    let sock = match fixed {
        None => Some(discovery_socket(&log)?),
        Some(_) => None,
    };
    let my_id = settings.machine_id.clone();
    let shared = Arc::new(SecondaryShared {
        connected_to: Mutex::new(None),
        link: Mutex::new(None),
        searching_since: Mutex::new(Some(Instant::now())),
        problem: Mutex::new(None),
        settings: Mutex::new(settings),
        dir: settings_dir,
        ignore_seq: Arc::new(Mutex::new(0u32)),
        options: options.clone(),
        log: log.clone(),
    });
    {
        let sh = shared.clone();
        spawn_clipboard_watcher(
            &my_id,
            options,
            shared.ignore_seq.clone(),
            move || sh.link.lock().unwrap().iter().cloned().collect(),
            log.clone(),
        );
    }
    let sh = shared.clone();
    thread::spawn(move || {
        log("Looking for the master PC...".to_string());
        let mut round = 0u32;
        loop {
            let found = match (&sock, fixed) {
                (Some(s), _) => find_master(s, &my_id, &sh, &mut round),
                (None, Some(a)) => {
                    let set = sh.settings.lock().unwrap();
                    // The id is known if this address worked before.
                    let id = if set.last_master_addr == a.to_string() {
                        set.last_master_id.clone()
                    } else {
                        String::new()
                    };
                    Found {
                        id,
                        addr: a,
                        guess: false,
                    }
                }
                _ => return,
            };
            let res = secondary_session(&sh, &found.id, found.addr);
            let was_connected = sh.connected_to.lock().unwrap().take().is_some();
            *sh.link.lock().unwrap() = None;
            match res {
                // A blind try of the last known address may fail quietly.
                Err(_) if found.guess && !was_connected => {}
                Err(e) if was_connected => log(format!("Disconnected: {e}")),
                Err(e) => log(format!("Could not connect to the master PC: {e}")),
                Ok(()) => {}
            }
            if was_connected {
                *sh.searching_since.lock().unwrap() = Some(Instant::now());
                log("Looking for the master PC...".to_string());
            }
            thread::sleep(Duration::from_secs(if was_connected { 1 } else { 2 }));
        }
    });
    Ok(SecondaryHandle { shared })
}

struct Found {
    id: String,
    addr: SocketAddr,
    /// True when nothing answered and the last known address is being tried.
    guess: bool,
}

/// Search the network until a master answers. Every few rounds without an
/// answer, the address where the master was last seen is offered instead.
fn find_master(sock: &UdpSocket, my_id: &str, sh: &SecondaryShared, round: &mut u32) -> Found {
    let host = host_name();
    let probe = probe_bytes(my_id);
    loop {
        *round = round.wrapping_add(1);
        let (last_addr, last_id) = {
            let s = sh.settings.lock().unwrap();
            (
                s.last_master_addr.parse::<SocketAddr>().ok(),
                s.last_master_id.clone(),
            )
        };
        let mut targets = broadcast_targets(&local_ipv4s(&host), DISCOVERY_PORT);
        if let Some(a) = last_addr {
            targets.push(SocketAddr::new(a.ip(), DISCOVERY_PORT));
        }
        send_to_all(sock, &probe, &targets);
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match recv_packet(sock, my_id, left) {
                Ok(Some((Packet::Beacon(b), from))) if b.is_master => {
                    if b.version != PROTOCOL_VERSION {
                        *sh.problem.lock().unwrap() = Some(format!(
                            "\"{}\" runs a different version of this program. Install the same version on every PC.",
                            b.name
                        ));
                        continue;
                    }
                    *sh.problem.lock().unwrap() = None;
                    return Found {
                        id: b.id,
                        addr: SocketAddr::new(from.ip(), b.port),
                        guess: false,
                    };
                }
                Ok(_) => {}
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
        if (*round).is_multiple_of(4) {
            if let Some(addr) = last_addr {
                return Found {
                    id: last_id,
                    addr,
                    guess: true,
                };
            }
        }
    }
}

/// Keys and buttons the master is holding down on this PC. If the connection
/// is lost they are released, so nothing stays stuck.
#[derive(Default)]
struct Pressed {
    keys: HashSet<KeyId>,
    buttons: HashSet<Button>,
}

impl Pressed {
    fn note(&mut self, m: &Message) {
        match *m {
            Message::Key {
                scancode,
                extended,
                down,
            } => {
                if down {
                    self.keys.insert((scancode, extended));
                } else {
                    self.keys.remove(&(scancode, extended));
                }
            }
            Message::MouseButton { button, down } => {
                if down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            _ => {}
        }
    }

    fn release_all(&mut self) {
        for (scancode, extended) in self.keys.drain() {
            platform::inject(&Message::Key {
                scancode,
                extended,
                down: false,
            });
        }
        for button in self.buttons.drain() {
            platform::inject(&Message::MouseButton {
                button,
                down: false,
            });
        }
    }
}

impl Drop for Pressed {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn secondary_session(sh: &SecondaryShared, master_id: &str, addr: SocketAddr) -> Result<()> {
    let try_connect = |pin: Option<&str>| -> Result<handshake::Established<TcpStream>> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(4))?;
        prepare(&stream)?;
        let (my, secret) = {
            let s = sh.settings.lock().unwrap();
            (me(&s), s.secret_for(master_id))
        };
        handshake::connect(stream, &my, master_id, secret, pin)
    };
    // Every PC uses the same fixed PIN, so there is nothing to ask the user.
    let est = match try_connect(None) {
        Err(AppError::PinRequired) => try_connect(Some(crate::SHARED_PIN))?,
        Err(AppError::PairingRejected) => {
            // The saved pairing no longer matches (the program was set up
            // again on one of the PCs): forget it and pair again with the PIN.
            {
                let mut s = sh.settings.lock().unwrap();
                s.forget(master_id);
                let _ = store::save_learnt(&sh.dir, &s, false);
            }
            try_connect(Some(crate::SHARED_PIN))?
        }
        other => other?,
    };
    let peer = est.peer.clone();
    {
        let mut s = sh.settings.lock().unwrap();
        let addr_text = addr.to_string();
        let mut changed = false;
        if let Some(secret) = est.new_secret {
            s.save_pairing(&peer.id, &peer.name, &secret);
            changed = true;
        }
        if s.last_master_addr != addr_text || s.last_master_id != peer.id {
            s.last_master_addr = addr_text;
            s.last_master_id = peer.id.clone();
            changed = true;
        }
        if changed {
            store::save_learnt(&sh.dir, &s, false)?;
        }
    }
    let (sender, mut receiver) = est.channel.split(|s| s.try_clone())?;
    let link = Link::open(sender);
    let mut size = platform::screen_size();
    link.queue(Message::ScreenInfo {
        width: size.0,
        height: size.1,
    });
    let mut incoming = Incoming::new(sh.ignore_seq.clone(), sh.options.clone())?;
    *sh.link.lock().unwrap() = Some(link.clone());
    *sh.connected_to.lock().unwrap() = Some(peer.name.clone());
    *sh.searching_since.lock().unwrap() = None;
    *sh.problem.lock().unwrap() = None;
    (sh.log)(format!("Connected to {}.", peer.name));

    let mut pressed = Pressed::default();
    let mut checked = Instant::now();
    let mut awake: Option<Instant> = None;
    loop {
        // The master sends a heartbeat every few seconds, so this loop turns
        // regularly even when nobody touches the mouse.
        let msg = receiver.recv()?;
        if checked.elapsed() >= Duration::from_secs(2) {
            checked = Instant::now();
            let now = platform::screen_size();
            if now != size && now.0 > 0 && now.1 > 0 {
                size = now;
                link.queue(Message::ScreenInfo {
                    width: now.0,
                    height: now.1,
                });
            }
        }
        if incoming.handle(&msg, &sh.log) {
            if let Message::FileEnd { .. } = msg {
                finish_batch(&mut incoming, &sh.log);
            }
            continue;
        }
        match msg {
            Message::Ping
            | Message::Pong
            | Message::PairingSecret(_)
            | Message::ScreenInfo { .. } => {}
            Message::Leave => pressed.release_all(),
            other => {
                if matches!(other, Message::Enter { .. }) {
                    // Switch a sleeping screen back on when the cursor arrives.
                    platform::wake_screen();
                    awake = Some(Instant::now());
                } else if awake.is_none_or(|t| t.elapsed() >= Duration::from_secs(20)) {
                    platform::keep_awake();
                    awake = Some(Instant::now());
                }
                pressed.note(&other);
                platform::inject(&other);
            }
        }
    }
}
