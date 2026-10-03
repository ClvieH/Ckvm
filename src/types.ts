export type Platform = 'windows' | 'macos' | 'unknown'

// 'peer' is the bidirectional role: the machine captures (controls others) and
// receives (is controlled) at the same time. Both sides of a peer pair run the
// full runtime, so either user can slide onto the other machine's screens.
export type MachineRole = 'unset' | 'server' | 'client' | 'peer'

export type AppLanguage = 'cn' | 'en'

export type ThemeMode = 'system' | 'dark' | 'light'

export type TransportPortMode = 'auto' | 'fixed'

export type ModifierTarget = 'control' | 'alt' | 'meta' | 'same'

export interface ModifierMap {
  control: ModifierTarget
  alt: ModifierTarget
  meta: ModifierTarget
}

export interface ScreenSwitchHotkeys {
  left: string
  right: string
  up: string
  down: string
}

export interface PairedController {
  id: string
  name: string
  host: string
  ip: string
  transportPublicKey: string
  protocolVersion: number
  clusterId: string
  pairedAtMs: number
}

export interface Screen {
  id: string
  deviceId: string
  name: string
  x: number
  y: number
  width: number
  height: number
  scale: number
  isPrimary: boolean
}

export interface Device {
  id: string
  name: string
  platform: Platform
  host: string
  // NIC MAC (colonless hex) for Wake-on-LAN; empty when the peer never
  // advertised one (older version).
  mac: string
  transportPort: number
  quicPort: number
  transportPublicKey: string
  protocolVersion: number
  color: string
  online: boolean
  inputReady: boolean
  upgrading?: boolean
  role: 'local' | 'server' | 'client'
  source?: 'detected' | 'manual'
  screens: Screen[]
}

export interface LayoutState {
  devices: Device[]
  activeDeviceId: string
  selectedScreenId: string
  // 'both' is the peer mode: capture local input AND accept remote input.
  inputMode: 'control' | 'receive' | 'both'
  machineRole: MachineRole
  clusterId: string
  pairSecret: string
  pairedControllers: PairedController[]
  clipboardSync: boolean
  fileTransferEnabled: boolean
  // Open pairing: discovered LAN peers are trusted and paired automatically
  // (anchored on their transport certificate). Off = confirmation-code flow.
  autoPairing: boolean
  // Lock this machine's screen when the user walks away with the cursor.
  lockOnLeave: boolean
  // Pause edge crossing while a fullscreen app (game/video) is foreground.
  fullscreenGuard: boolean
  // Global hotkey opening the clipboard-history popup ("" disables it).
  clipboardHistoryShortcut: string
  // Win→Win edge drags open a native OLE session on the receiver so files
  // drop into the folder under the cursor (off = Transfers folder).
  dragNativeDrop: boolean
  // Corner guard: refuse edge crossings starting inside a dead zone around the
  // local screen's four corners (window close buttons, Start menu...).
  cornerGuard: boolean
  cornerGuardSize: number
  language: AppLanguage
  themeMode: ThemeMode
  performanceMonitor: boolean
  transportPortMode: TransportPortMode
  transportPort: number
  quicPort: number
  modifierRemap: boolean
  modifierMap: ModifierMap
  edgeSwitchHotkey: string
  screenSwitchHotkeys: ScreenSwitchHotkeys
}
