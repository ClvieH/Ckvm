import { describe, expect, it } from 'vitest'

import {
  flattenScreens,
  getLayoutBounds,
  getScreenById,
  moveScreen,
  screenPositionOverlaps,
  snapOverlappingScreenPosition,
  snapScreenPosition,
} from './layout'
import type { Device, LayoutState, Screen } from './types'

function makeScreen(overrides: Partial<Screen> & Pick<Screen, 'id'>): Screen {
  return {
    deviceId: 'device-a',
    name: overrides.id,
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
    scale: 1,
    isPrimary: false,
    ...overrides,
  }
}

function makeLayout(devices: Device[]): LayoutState {
  return {
    devices,
    activeDeviceId: devices[0]?.id ?? '',
    selectedScreenId: '',
    inputMode: 'control',
    machineRole: 'server',
    clipboardHistoryShortcut: 'ctrl+shift+v',
    clusterId: 'cluster-test',
    pairSecret: 'secret-test',
    pairedControllers: [],
    clipboardSync: false,
    fileTransferEnabled: true,
    autoPairing: true,
    lockOnLeave: false,
    fullscreenGuard: true,
    cornerGuard: true,
    cornerGuardSize: 32,
    language: 'cn',
    themeMode: 'system',
    performanceMonitor: false,
    transportPortMode: 'auto',
    transportPort: 47833,
    quicPort: 47834,
    modifierRemap: true,
    modifierMap: { control: 'meta', alt: 'same', meta: 'control' },
    edgeSwitchHotkey: 'alt+shift+k',
    screenSwitchHotkeys: { left: 'alt+left', right: 'alt+right', up: 'alt+up', down: 'alt+down' },
  }
}

function device(id: string, screens: Screen[]): Device {
  return {
    id,
    name: id,
    platform: 'windows',
    mac: 'aabbccddeeff',
    host: `${id}.local`,
    transportPort: 47833,
    quicPort: 47834,
    transportPublicKey: '',
    protocolVersion: 1,
    color: '#2f7af8',
    online: true,
    inputReady: true,
    role: id === 'device-a' ? 'local' : 'server',
    screens,
  }
}

const baseLayout = makeLayout([
  device('device-a', [makeScreen({ id: 'a-1' })]),
  device('device-b', [makeScreen({ id: 'b-1', deviceId: 'device-b', x: 1920 })]),
])

describe('flattenScreens', () => {
  it('flattens every device screen with device metadata attached', () => {
    const flattened = flattenScreens(baseLayout)

    expect(flattened).toHaveLength(2)
    expect(flattened.map((screen) => screen.id)).toEqual(['a-1', 'b-1'])
    expect(flattened[1]).toMatchObject({
      deviceName: 'device-b',
      deviceId: 'device-b',
      online: true,
      inputReady: true,
    })
  })
})

describe('getLayoutBounds', () => {
  it('returns the bounding box of all screens', () => {
    const bounds = getLayoutBounds(flattenScreens(baseLayout))

    expect(bounds).toEqual({ minX: 0, minY: 0, maxX: 3840, maxY: 1080, width: 3840, height: 1080 })
  })

  it('never collapses to a zero-size box', () => {
    const bounds = getLayoutBounds([makeScreen({ id: 's' })])

    expect(bounds.width).toBeGreaterThan(0)
    expect(bounds.height).toBeGreaterThan(0)
  })
})

describe('screenPositionOverlaps', () => {
  it('detects a move onto an existing screen', () => {
    expect(screenPositionOverlaps(baseLayout, 'a-1', { x: 1920, y: 0 })).toBe(true)
  })

  it('allows a side-by-side placement', () => {
    expect(screenPositionOverlaps(baseLayout, 'a-1', { x: -1920, y: 0 })).toBe(false)
  })

  it('ignores the screen being moved', () => {
    expect(screenPositionOverlaps(baseLayout, 'b-1', { x: 1920, y: 0 })).toBe(false)
  })
})

describe('snapOverlappingScreenPosition', () => {
  it('snaps an overlapping move to the nearest side of the dominant overlap', () => {
    const snapped = snapOverlappingScreenPosition(
      baseLayout,
      'a-1',
      { x: 2000, y: 100 },
      { x: 0, y: 0 },
    )

    // The moving screen's center ends up right of b-1's center, so it snaps
    // flush against b-1's right edge.
    expect(snapped).toEqual({ x: 3840, y: 100 })
  })

  it('returns null when even the snapped position would overlap', () => {
    // b-1 blocks the left half and b-2 blocks the snapped-right position, so
    // no side is free and the snap gives up.
    const blocked = makeLayout([
      device('device-a', [makeScreen({ id: 'a-1' })]),
      device('device-b', [
        makeScreen({ id: 'b-1', deviceId: 'device-b', x: -960, width: 1920 }),
        makeScreen({ id: 'b-2', deviceId: 'device-b', x: 1920 }),
      ]),
    ])

    expect(snapOverlappingScreenPosition(blocked, 'a-1', { x: 0, y: 0 }, { x: 0, y: 0 })).toBeNull()
  })

  it('returns null when nothing overlaps', () => {
    expect(
      snapOverlappingScreenPosition(baseLayout, 'a-1', { x: 0, y: 0 }, { x: 0, y: 0 }),
    ).toBeNull()
  })
})

describe('snapScreenPosition', () => {
  it('snaps to a screen edge within the tolerance', () => {
    // 30px right of b-1's left edge (1920) is within the 80px tolerance.
    expect(snapScreenPosition(baseLayout, 'a-1', { x: 1890, y: 0 })).toEqual({
      x: 1920,
      y: 0,
    })
  })

  it('keeps the position when it is outside the tolerance', () => {
    expect(snapScreenPosition(baseLayout, 'a-1', { x: 1500, y: 0 })).toEqual({
      x: 1500,
      y: 0,
    })
  })
})

describe('moveScreen', () => {
  it('updates only the target screen position', () => {
    const moved = moveScreen(baseLayout, 'b-1', { x: 3840, y: 2160 })
    const movedB1 = getScreenById(moved, 'b-1')

    expect(movedB1).toMatchObject({ x: 3840, y: 2160 })
    expect(getScreenById(moved, 'a-1')).toMatchObject({ x: 0, y: 0 })
    expect(moved).not.toBe(baseLayout)
  })
})

describe('getScreenById', () => {
  it('finds screens across devices', () => {
    expect(getScreenById(baseLayout, 'b-1')?.deviceId).toBe('device-b')
    expect(getScreenById(baseLayout, 'missing')).toBeUndefined()
  })
})
