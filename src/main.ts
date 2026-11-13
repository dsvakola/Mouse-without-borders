import "./style.css";
import logoUrl from "./assets/logo.png";
import { allowFirewall, applySettings, getAppInfo, getState, hideWindow, openLink } from "./api";
import type { AppState, Cell, Machine, Role } from "./types";

const COLS = 3;

function $<T extends HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing element #${id}`);
  return el as T;
}

let state: AppState | null = null;
/** Arrangement as the user sees it (may differ from the saved one until Apply). */
let cells = new Map<string, { col: number; row: number }>();
let dirty = false;
let formLoaded = false;

const matrix = $("matrix");
const secondaryCard = $("secondary-card");
const statusEl = $("status");
const twoRow = $<HTMLInputElement>("two-row");

function role(): Role {
  const r = document.querySelector<HTMLInputElement>('input[name="role"]:checked');
  return r?.value === "secondary" ? "secondary" : "master";
}

function rows(): number {
  return twoRow.checked ? 2 : 1;
}

function monitorSvg(connected: boolean): string {
  const a = connected ? "#5cb3ff" : "#b8b8b8";
  const b = connected ? "#0a5bb5" : "#7d7d7d";
  return `<svg viewBox="0 0 64 52" width="64" height="52" aria-hidden="true">
    <defs><linearGradient id="g${connected ? 1 : 0}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs>
    <rect x="3" y="2" width="58" height="38" rx="3" fill="#2b2b2b"/>
    <rect x="6" y="5" width="52" height="32" rx="1.5" fill="url(#g${connected ? 1 : 0})"/>
    <rect x="26" y="40" width="12" height="6" fill="#6b6b6b"/>
    <rect x="17" y="46" width="30" height="4" rx="2" fill="#8a8a8a"/></svg>`;
}

function renderMatrix(): void {
  if (!state) return;
  const isMaster = role() === "master";
  matrix.hidden = !isMaster;
  secondaryCard.hidden = isMaster;
  twoRow.parentElement!.hidden = !isMaster;
  $("matrix-hint").hidden = !isMaster;
  if (!isMaster) {
    const host = state.connectedTo;
    const waiting = state.role === "secondary" ? "Looking for the master PC on the network…" : "Press Apply to make this computer a secondary.";
    secondaryCard.classList.toggle("warn", !host && !!state.warning);
    secondaryCard.textContent = host
      ? `Connected to ${host}. Move the mouse on the master PC to control this computer.`
      : state.role === "secondary" && state.warning ? state.warning : waiting;
    return;
  }
  matrix.style.gridTemplateRows = `repeat(${rows()}, 1fr)`;
  matrix.classList.toggle("two", rows() === 2);
  matrix.innerHTML = "";
  const byCell = new Map<string, Machine>();
  for (const m of state.machines) {
    const c = cells.get(m.id) ?? { col: m.col, row: m.row };
    byCell.set(`${c.col},${c.row}`, m);
  }
  for (let r = 0; r < rows(); r++) {
    for (let c = 0; c < COLS; c++) {
      const cell = document.createElement("div");
      cell.className = "cell";
      cell.dataset.col = String(c);
      cell.dataset.row = String(r);
      const m = byCell.get(`${c},${r}`);
      if (m) cell.append(thumb(m));
      matrix.append(cell);
    }
  }
}

function thumb(m: Machine): HTMLElement {
  const t = document.createElement("div");
  t.className = "thumb" + (m.connected ? "" : " off");
  t.tabIndex = 0;
  t.dataset.id = m.id;
  t.setAttribute("aria-label", `${m.name}, drag to move`);
  const label = m.isLocal ? "this computer" : m.connected ? "Connected" : "Not connected";
  t.innerHTML = `${monitorSvg(m.connected)}<div class="name"></div><div class="sub">${label}</div>`;
  t.querySelector(".name")!.textContent = m.name;
  t.addEventListener("pointerdown", (e) => startDrag(e, t, m.id));
  t.addEventListener("keydown", (e) => keyMove(e, m.id));
  return t;
}

function place(id: string, col: number, row: number): void {
  if (!state) return;
  const current = (mid: string) => {
    const m = state!.machines.find((x) => x.id === mid)!;
    return cells.get(mid) ?? { col: m.col, row: m.row };
  };
  const from = current(id);
  const other = state.machines.find((m) => m.id !== id && current(m.id).col === col && current(m.id).row === row);
  cells.set(id, { col, row });
  if (other) cells.set(other.id, from); // swap places
  dirty = true;
  renderMatrix();
  setStatus("Arrangement changed. Press Apply to save.");
}

function startDrag(e: PointerEvent, el: HTMLElement, id: string): void {
  if (e.button !== 0) return;
  const ghost = el.cloneNode(true) as HTMLElement;
  ghost.classList.add("ghost");
  document.body.append(ghost);
  el.classList.add("dragging");
  const move = (ev: PointerEvent) => {
    ghost.style.left = `${ev.clientX - ghost.offsetWidth / 2}px`;
    ghost.style.top = `${ev.clientY - ghost.offsetHeight / 2}px`;
    const over = cellAt(ev.clientX, ev.clientY);
    matrix.querySelectorAll(".cell").forEach((c) => c.classList.toggle("target", c === over));
  };
  const up = (ev: PointerEvent) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    ghost.remove();
    const target = cellAt(ev.clientX, ev.clientY);
    if (target) place(id, Number(target.dataset.col), Number(target.dataset.row));
    else renderMatrix();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  move(e);
  e.preventDefault();
}

function cellAt(x: number, y: number): HTMLElement | null {
  const el = document.elementFromPoint(x, y);
  return el?.closest<HTMLElement>(".cell") ?? null;
}

function keyMove(e: KeyboardEvent, id: string): void {
  const d: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
  const step = d[e.key];
  if (!step || !state) return;
  const m = state.machines.find((x) => x.id === id)!;
  const c = cells.get(id) ?? { col: m.col, row: m.row };
  const col = c.col + step[0];
  const row = c.row + step[1];
  if (col < 0 || col >= COLS || row < 0 || row >= rows()) return;
  e.preventDefault();
  place(id, col, row);
  matrix.querySelector<HTMLElement>(`.thumb[data-id="${CSS.escape(id)}"]`)?.focus();
}

/** Unticking "Two Row" moves computers from row 2 into free places in row 1. */
function packIntoOneRow(): void {
  if (!state) return;
  const pos = (m: Machine) => cells.get(m.id) ?? { col: m.col, row: m.row };
  const used = new Set(state.machines.filter((m) => pos(m).row === 0).map((m) => pos(m).col));
  for (const m of state.machines.filter((x) => pos(x).row !== 0)) {
    const free = [0, 1, 2].find((c) => !used.has(c));
    if (free === undefined) return;
    used.add(free);
    cells.set(m.id, { col: free, row: 0 });
  }
}

function setStatus(text: string, error = false): void {
  statusEl.textContent = text;
  statusEl.title = text;
  statusEl.classList.toggle("error", error);
}

function summary(): void {
  if (!state || dirty) return;
  if (state.engineError) return setStatus(state.engineError, true);
  if (state.role === "secondary") {
    if (state.connectedTo) return setStatus(`Connected to ${state.connectedTo}`);
    return state.warning ? setStatus(state.warning, true) : setStatus("Looking for the master PC…");
  }
  if (state.warning) return setStatus(state.warning, true);
  const others = state.machines.filter((m) => !m.isLocal && m.connected).length;
  setStatus(others === 0 ? "Ready. Waiting for other computers to connect." : `${others} other computer${others === 1 ? "" : "s"} connected.`);
}

function loadForm(s: AppState): void {
  document.querySelector<HTMLInputElement>(`input[name="role"][value="${s.role}"]`)!.checked = true;
  $<HTMLInputElement>("pin").value = s.sharedPin;
  twoRow.checked = s.twoRow || s.machines.some((m) => m.row > 0);
  $<HTMLInputElement>("opt-clipboard").checked = s.shareClipboard;
  $<HTMLInputElement>("opt-files").checked = s.transferFiles;
  $<HTMLInputElement>("master-ip").value = s.masterIp;
  $<HTMLInputElement>("opt-autostart").checked = s.startWithWindows;
  formLoaded = true;
}

async function refresh(): Promise<void> {
  try {
    state = await getState();
  } catch (e) {
    setStatus(String(e), true);
    return;
  }
  if (!formLoaded) loadForm(state);
  if (!dirty) cells = new Map();
  if (!document.querySelector(".thumb.dragging")) renderMatrix();
  $("my-address").textContent = state.addresses.length ? state.addresses.join("  or  ") : "not connected to a network";
  summary();
}

async function apply(): Promise<void> {
  if (!state) return;
  const list: Cell[] = state.machines.map((m) => {
    const c = cells.get(m.id) ?? { col: m.col, row: m.row };
    return { id: m.id, col: c.col, row: c.row };
  });
  try {
    const restarting = await applySettings({
      role: role(),
      shareClipboard: $<HTMLInputElement>("opt-clipboard").checked,
      transferFiles: $<HTMLInputElement>("opt-files").checked,
      masterIp: $<HTMLInputElement>("master-ip").value,
      startWithWindows: $<HTMLInputElement>("opt-autostart").checked,
      twoRow: twoRow.checked,
      cells: list,
    });
    dirty = false;
    if (restarting) return setStatus("Restarting with the new settings…");
    setStatus("Settings saved.");
    await refresh();
  } catch (e) {
    setStatus(String(e), true);
  }
}

async function showAbout(): Promise<void> {
  const info = await getAppInfo();
  $<HTMLImageElement>("about-logo").src = logoUrl;
  $("about-name").textContent = info.name;
  $("about-version").textContent = `Version ${info.version}`;
  $("about-dev").textContent = `Developed by ${info.developers}`;
  $("about-org").textContent = `for ${info.organisation}`;
  $("about-site").textContent = info.website;
  $("about-site").onclick = () => void openLink(info.website);
  $("about-mail").textContent = info.email;
  $("about-mail").onclick = () => void openLink(`mailto:${info.email}`);
  $("about-license").textContent = `Licensed under the ${info.license}`;
  $<HTMLDialogElement>("about").showModal();
}

function wire(): void {
  document.querySelectorAll<HTMLButtonElement>(".tab").forEach((tab) =>
    tab.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
      for (const name of ["setup", "options", "ip"]) $(`panel-${name}`).hidden = name !== tab.dataset.tab;
    }),
  );
  document.querySelectorAll('input[name="role"]').forEach((r) => r.addEventListener("change", () => { dirty = true; renderMatrix(); }));
  $<HTMLInputElement>("show-pin").addEventListener("change", (e) => {
    $<HTMLInputElement>("pin").type = (e.target as HTMLInputElement).checked ? "text" : "password";
  });
  twoRow.addEventListener("change", () => {
    if (!twoRow.checked) packIntoOneRow();
    dirty = true;
    renderMatrix();
  });
  $("firewall").addEventListener("click", () => {
    allowFirewall().then(
      () => { $("firewall-note").textContent = "Permission requested. If you answered Yes, this PC is now allowed on every network."; },
      (e) => { $("firewall-note").textContent = String(e); },
    );
  });
  for (const id of ["opt-clipboard", "opt-files", "opt-autostart", "master-ip"]) $(id).addEventListener("input", () => { dirty = true; });
  $("apply").addEventListener("click", () => void apply());
  $("close").addEventListener("click", () => void hideWindow());
  $("about-link").addEventListener("click", () => void showAbout());
}

wire();
void refresh();
setInterval(() => void refresh(), 1500);
