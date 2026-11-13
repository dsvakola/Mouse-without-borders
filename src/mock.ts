// Development only: lets the screen be previewed in a plain browser.
import type { AppInfo, AppState, ApplyRequest } from "./types";

let state: AppState = {
  role: "master",
  machineName: "seniorTeacher",
  sharedPin: "123456",
  twoRow: false,
  shareClipboard: true,
  transferFiles: true,
  masterIp: "",
  startWithWindows: true,
  machines: [
    { id: "a", name: "seniorTeacher", isLocal: true, connected: true, col: 0, row: 0 },
    { id: "b", name: "OLDLAPTOP", isLocal: false, connected: true, col: 1, row: 0 },
    { id: "c", name: "OFFICE-PC", isLocal: false, connected: false, col: 2, row: 0 },
  ],
  connectedTo: null,
  engineError: null,
  warning: null,
  addresses: ["192.168.1.7"],
  log: ["OLDLAPTOP connected (1366x768)."],
};

export function mockState(): AppState {
  return structuredClone(state);
}

export function mockApply(req: ApplyRequest): boolean {
  state = {
    ...state,
    role: req.role,
    shareClipboard: req.shareClipboard,
    transferFiles: req.transferFiles,
    masterIp: req.masterIp,
    startWithWindows: req.startWithWindows,
    twoRow: req.twoRow,
    machines: state.machines.map((m) => {
      const c = req.cells.find((x) => x.id === m.id);
      return c ? { ...m, col: c.col, row: c.row } : m;
    }),
  };
  return false;
}

export function mockInfo(): AppInfo {
  return {
    name: "Mouse without borders",
    version: "0.2.0",
    developers: "Prof. Dattaraj S. Vidyasagar & Prof. Yash D. Vidyasagar",
    organisation: "Vidyasagar Academy",
    website: "https://vsa.edu.in/software/",
    email: "softinquiry@vsa.edu.in",
    license: "GNU General Public License v3.0",
  };
}
