//! Runs a master or a secondary PC: connections, input sharing, clipboard and files.
//! Windows only. Everything here is driven by the tested core modules.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::channel::SecureReceiver;
use crate::discovery::{
    announce, listen_socket, wait_for_beacon, Beacon, CONNECT_PORT, DISCOVERY_PORT,
};
use crate::files::FileReceiver;
use crate::handshake::{self, PeerInfo};
use crate::layout::{layout_from_cells, Layout, Placed, Screen, GRID_COLS, GRID_ROWS};
use crate::limiter::AttemptLimiter;
use crate::platform::{self, Captured};
use crate::protocol::{Message, FILE_CHUNK};
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
const PING_EVERY: Duration = Duration::from_secs(3);
const DEAD_AFTER: Duration = Duration::from_secs(10);

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
    for _ in 0..15 {
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

type Tx = SyncSender<Message>;

fn queue(tx: &Tx, m: Message) {
    match tx.try_send(m) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {} // peer is stuck; the heartbeat will drop it
    }
}

/// Writer thread: sends queued messages, plus a heartbeat.
fn spawn_writer(mut sender: crate::channel::SecureSender<TcpStream>, rx: Receiver<Message>) {
    thread::spawn(move || loop {
        match rx.recv_timeout(PING_EVERY) {
            Ok(m) => {
                if sender.send(&m).is_err() {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if sender.send(&Message::Ping).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    });
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
        let is_file = matches!(
            msg,
            Message::FileOffer { .. } | Message::FileChunk { .. } | Message::FileEnd { .. }
        );
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
fn send_file(
    tx: &SyncSender<Message>,
    path: &PathBuf,
    id: u64,
    index: u32,
    total: u32,
) -> Result<()> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    tx.send(Message::FileOffer {
        id,
        name,
        size,
        index,
        total,
    })
    .map_err(|_| AppError::Io("connection closed".into()))?;
    let mut buf = vec![0u8; FILE_CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        tx.send(Message::FileChunk {
            id,
            data: buf[..n].to_vec(),
        })
        .map_err(|_| AppError::Io("connection closed".into()))?;
    }
    tx.send(Message::FileEnd { id })
        .map_err(|_| AppError::Io("connection closed".into()))
}

/// Watches the local clipboard and sends changes to `targets()`.
fn spawn_clipboard_watcher(
    options: Arc<Options>,
    ignore_seq: Arc<Mutex<u32>>,
    targets: impl Fn() -> Vec<Tx> + Send + 'static,
    log: Log,
) {
    thread::spawn(move || {
        let mut last = platform::clipboard_sequence();
        let mut next_id = 1u64;
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
                    next_id += 1;
                    for tx in &peers {
                        if let Err(e) = send_file(tx, f, next_id, i as u32, total) {
                            log(format!("Could not send {}: {e}", f.display()));
                        }
                    }
                }
            } else if !options.clipboard.load(Ordering::Relaxed) {
                continue;
            } else if let Some(t) = platform::get_clipboard_text() {
                for tx in &peers {
                    queue(tx, Message::ClipboardText(t.clone()));
                }
            }
        }
    });
}

// ------------------------------------------------------------------- master

struct Peer {
    tx: Tx,
    name: String,
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
}

impl MasterState {
    fn apply(&mut self, actions: Vec<Action>, log: &Log) {
        for a in actions {
            match a {
                Action::Leave { id } if id == self.local_id => {
                    self.grabbed = true;
                    platform::set_cursor_pos(self.center.0, self.center.1);
                }
                Action::Leave { id } => {
                    if let Some(p) = self.peers.get(&id) {
                        queue(&p.tx, Message::Leave);
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
                        queue(&p.tx, Message::Enter { x, y });
                    }
                }
                Action::Forward { .. } => {
                    // Send where the cursor should be, not how far it moved: the
                    // other PC's pointer speed settings would otherwise make the
                    // real cursor drift away from what the router tracks.
                    let (x, y) = self.router.position();
                    if let Some(p) = self.peers.get(self.router.active()) {
                        queue(&p.tx, Message::MouseAbs { x, y });
                    }
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

fn hook_handler(shared: Arc<MasterShared>, log: Log) -> impl Fn(Captured) -> bool + Send + Sync {
    move |ev| {
        let mut st = shared.state.lock().unwrap();
        match ev {
            Captured::Key { scancode, down, .. }
                if platform::ctrl_alt_down() && (0x3B..=0x3D).contains(&scancode) =>
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
                if !st.grabbed {
                    st.router.sync_local(x, y);
                    let (w, h) = st.local_size;
                    let dx = if x >= w - 1 {
                        1
                    } else if x <= 0 {
                        -1
                    } else {
                        0
                    };
                    let dy = if y >= h - 1 {
                        1
                    } else if y <= 0 {
                        -1
                    } else {
                        0
                    };
                    if dx != 0 || dy != 0 {
                        let acts = st.router.feed_delta(dx, dy);
                        st.apply(acts, &log);
                    }
                    false
                } else {
                    let (dx, dy) = (x - st.center.0, y - st.center.1);
                    if dx != 0 || dy != 0 {
                        let acts = st.router.feed_delta(dx, dy);
                        st.apply(acts, &log);
                        if st.grabbed {
                            platform::set_cursor_pos(st.center.0, st.center.1);
                        }
                    }
                    true
                }
            }
            other if st.grabbed => {
                let msg = match other {
                    Captured::MouseButton { button, down } => Message::MouseButton { button, down },
                    Captured::Wheel { dx, dy } => Message::Wheel { dx, dy },
                    Captured::Key {
                        scancode,
                        extended,
                        down,
                    } => Message::Key {
                        scancode,
                        extended,
                        down,
                    },
                    Captured::MouseMove { .. } => return true,
                };
                if let Some(p) = st.peers.get(st.router.active()) {
                    queue(&p.tx, msg);
                }
                true
            }
            _ => false,
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
        let mut st = self.shared.state.lock().unwrap();
        let old = st.cells.clone();
        for c in &cells {
            st.cells.insert(c.id.clone(), (c.col, c.row));
        }
        if let Err(e) = st.rebuild_layout() {
            st.cells = old;
            return Err(e);
        }
        let mut set = self.shared.settings.lock().unwrap();
        set.cells = st
            .cells
            .iter()
            .map(|(id, (col, row))| Cell {
                id: id.clone(),
                col: *col,
                row: *row,
            })
            .collect();
        set.two_row = two_row;
        store::save(&self.shared.dir, &set)
    }

    pub fn options(&self) -> Arc<Options> {
        self.shared.options.clone()
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
    });

    // The hook handler needs the state alone, so give it a small adapter.
    let hook_shared = shared.clone();
    platform::start_capture(hook_handler(hook_shared, log.clone()))?;

    // Tell the network we are here.
    {
        let beacon = Beacon::new(&settings.machine_id, &settings.machine_name, true);
        let target: SocketAddr = ([255, 255, 255, 255], DISCOVERY_PORT).into();
        thread::spawn(move || loop {
            let _ = announce(&beacon, target);
            thread::sleep(Duration::from_secs(2));
        });
    }

    // Clipboard from this PC goes to every secondary.
    {
        let sh = shared.clone();
        spawn_clipboard_watcher(
            options,
            shared.ignore_seq.clone(),
            move || {
                sh.state
                    .lock()
                    .unwrap()
                    .peers
                    .values()
                    .map(|p| p.tx.clone())
                    .collect()
            },
            log.clone(),
        );
    }

    let listener = retry(|| TcpListener::bind(("0.0.0.0", CONNECT_PORT))).map_err(|e| {
        AppError::Io(format!(
            "cannot listen on port {CONNECT_PORT} (is another copy running?): {e}"
        ))
    })?;
    log(format!(
        "Master \"{}\" is ready. Hotkeys: Ctrl+Alt+F1 = this PC, F2/F3 = other PCs.",
        settings.machine_name
    ));
    let accept_shared = shared.clone();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
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

fn master_connection(stream: TcpStream, sh: &Arc<MasterShared>) -> Result<()> {
    let ip: IpAddr = stream.peer_addr()?.ip();
    if let Err(secs) = sh.limiter.lock().unwrap().check(ip, Instant::now()) {
        return Err(AppError::LockedOut(secs));
    }
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(DEAD_AFTER))?;
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
            if matches!(e, AppError::WrongPin | AppError::PairingRejected) {
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
        store::save(&sh.dir, &s)?;
        log(format!("{} paired successfully.", est.peer.name));
    }
    let peer = est.peer;
    let (sender, mut receiver) = est.channel.split(|s| s.try_clone())?;

    // First message from a secondary: its screen size.
    let (w, h) = match receiver.recv()? {
        Message::ScreenInfo { width, height } if width > 0 && height > 0 => (width, height),
        _ => return Err(AppError::Protocol("screen size missing".into())),
    };
    let (tx, rx) = sync_channel::<Message>(1024);
    spawn_writer(sender, rx);
    {
        let mut st = sh.state.lock().unwrap();
        if st.sizes.len() >= crate::layout::MAX_MACHINES && !st.sizes.iter().any(|s| s.0 == peer.id)
        {
            return Err(AppError::InvalidLayout(
                "this master already has 3 computers".into(),
            ));
        }
        st.sizes.retain(|s| s.0 != peer.id);
        st.sizes.push((peer.id.clone(), w, h));
        st.peers.insert(
            peer.id.clone(),
            Peer {
                tx: tx.clone(),
                name: peer.name.clone(),
            },
        );
        if let Err(e) = st.rebuild_layout() {
            st.sizes.retain(|s| s.0 != peer.id);
            st.peers.remove(&peer.id);
            return Err(e);
        }
        let mut set = sh.settings.lock().unwrap();
        set.cells = st
            .cells
            .iter()
            .map(|(id, (col, row))| Cell {
                id: id.clone(),
                col: *col,
                row: *row,
            })
            .collect();
        let _ = store::save(&sh.dir, &set);
    }
    log(format!("{} connected ({}x{}).", peer.name, w, h));

    let mut incoming = Incoming::new(sh.ignore_seq.clone(), sh.options.clone())?;
    let outcome = master_receive(&mut receiver, &mut incoming, &sh.state, &peer, log);

    // Connection gone: take the cursor back if this PC had it.
    let mut st = sh.state.lock().unwrap();
    st.peers.remove(&peer.id);
    let acts = st.router.machine_lost(&peer.id);
    st.apply(acts, log);
    st.sizes.retain(|s| s.0 != peer.id);
    let _ = st.rebuild_layout();
    log(format!("{} disconnected.", peer.name));
    outcome
}

fn master_receive(
    receiver: &mut SecureReceiver<TcpStream>,
    incoming: &mut Incoming,
    state: &Mutex<MasterState>,
    peer: &PeerInfo,
    log: &Log,
) -> Result<()> {
    loop {
        let msg = receiver.recv()?;
        if incoming.handle(&msg, log) {
            // Pass what a secondary copied on to the other secondaries.
            if matches!(msg, Message::ClipboardText(_)) {
                for (id, p) in state.lock().unwrap().peers.iter() {
                    if *id != peer.id {
                        queue(&p.tx, msg.clone());
                    }
                }
            }
            if let Message::FileEnd { .. } = msg {
                finish_batch(incoming, log);
            }
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

#[derive(Clone)]
pub struct SecondaryHandle {
    connected_to: Arc<Mutex<Option<String>>>,
    options: Arc<Options>,
}

impl SecondaryHandle {
    /// Name of the master we are connected to, if any.
    pub fn connected_to(&self) -> Option<String> {
        self.connected_to.lock().unwrap().clone()
    }

    pub fn options(&self) -> Arc<Options> {
        self.options.clone()
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
        Some(
            format!("{ip}:{CONNECT_PORT}")
                .parse()
                .map_err(|_| AppError::Io(format!("\"{ip}\" is not a valid address")))?,
        )
    };
    let sock = if fixed.is_none() {
        Some(retry(|| listen_socket(DISCOVERY_PORT))?)
    } else {
        None
    };
    let my_id = settings.machine_id.clone();
    let settings = Arc::new(Mutex::new(settings));
    let ignore_seq = Arc::new(Mutex::new(0u32));
    let current_tx: Arc<Mutex<Option<Tx>>> = Arc::new(Mutex::new(None));
    let connected_to: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let ct = current_tx.clone();
        spawn_clipboard_watcher(
            options.clone(),
            ignore_seq.clone(),
            move || ct.lock().unwrap().iter().cloned().collect(),
            log.clone(),
        );
    }
    let handle = SecondaryHandle {
        connected_to: connected_to.clone(),
        options: options.clone(),
    };
    thread::spawn(move || {
        log("Looking for the master PC...".to_string());
        loop {
            let (master_id, addr) = match (&sock, fixed) {
                (Some(s), _) => match wait_for_beacon(s, &my_id, Duration::from_secs(2)) {
                    Ok(Some((b, from))) if b.is_master => {
                        (b.id, SocketAddr::new(from.ip(), b.port))
                    }
                    _ => continue,
                },
                (None, Some(a)) => (String::new(), a),
                _ => return,
            };
            let ctx = SecondaryCtx {
                dir: &settings_dir,
                settings: &settings,
                ignore_seq: &ignore_seq,
                options: &options,
                current_tx: &current_tx,
                connected_to: &connected_to,
                log: &log,
            };
            let res = secondary_session(&ctx, &master_id, addr);
            *current_tx.lock().unwrap() = None;
            *connected_to.lock().unwrap() = None;
            if let Err(e) = res {
                log(format!("Disconnected: {e}"));
            }
            thread::sleep(Duration::from_secs(2));
            log("Looking for the master PC...".to_string());
        }
    });
    Ok(handle)
}

struct SecondaryCtx<'a> {
    dir: &'a std::path::Path,
    settings: &'a Arc<Mutex<Settings>>,
    ignore_seq: &'a Arc<Mutex<u32>>,
    options: &'a Arc<Options>,
    current_tx: &'a Arc<Mutex<Option<Tx>>>,
    connected_to: &'a Arc<Mutex<Option<String>>>,
    log: &'a Log,
}

fn secondary_session(c: &SecondaryCtx, master_id: &str, addr: SocketAddr) -> Result<()> {
    let try_connect = |pin: Option<&str>| -> Result<handshake::Established<TcpStream>> {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(DEAD_AFTER))?;
        let (my, secret) = {
            let s = c.settings.lock().unwrap();
            (me(&s), s.secret_for(master_id))
        };
        handshake::connect(stream, &my, master_id, secret, pin)
    };
    // Every PC uses the same fixed PIN, so there is nothing to ask the user.
    let est = match try_connect(None) {
        Err(AppError::PinRequired) => try_connect(Some(crate::SHARED_PIN))?,
        other => other?,
    };
    let peer = est.peer.clone();
    if let Some(secret) = est.new_secret {
        let mut s = c.settings.lock().unwrap();
        s.save_pairing(&peer.id, &peer.name, &secret);
        store::save(c.dir, &s)?;
    }
    let (sender, mut receiver) = est.channel.split(|s| s.try_clone())?;
    let (tx, rx) = sync_channel::<Message>(1024);
    spawn_writer(sender, rx);
    let (w, h) = platform::screen_size();
    queue(
        &tx,
        Message::ScreenInfo {
            width: w,
            height: h,
        },
    );
    *c.current_tx.lock().unwrap() = Some(tx);
    *c.connected_to.lock().unwrap() = Some(peer.name.clone());
    (c.log)(format!("Connected to {}.", peer.name));

    let mut incoming = Incoming::new(c.ignore_seq.clone(), c.options.clone())?;
    loop {
        let msg = receiver.recv()?;
        if incoming.handle(&msg, c.log) {
            if let Message::FileEnd { .. } = msg {
                finish_batch(&mut incoming, c.log);
            }
            continue;
        }
        match msg {
            Message::Ping
            | Message::Pong
            | Message::Leave
            | Message::PairingSecret(_)
            | Message::ScreenInfo { .. } => {}
            other => platform::inject(&other),
        }
    }
}
