import { describe, expect, it } from 'vitest'

import {
  formatEdgeSwitchHotkeyForDisplay,
  hotkeyFromKeyboardEvent,
  metaKeyLabelForPlatform,
} from './hotkeyInput'
import type { HotkeyKeyboardEventLike } from './hotkeyInput'

function keyEvent(
  overrides: Partial<HotkeyKeyboardEventLike> & Pick<HotkeyKeyboardEventLike, 'key'>,
): HotkeyKeyboardEventLike {
  return {
    code: 'KeyA',
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ...overrides,
  }
}

describe('hotkeyFromKeyboardEvent', () => {
  it('canonicalizes letter codes with modifiers', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'a', ctrlKey: true, altKey: true }))).toBe(
      'ctrl+alt+a',
    )
    expect(
      hotkeyFromKeyboardEvent(keyEvent({ key: 'A', code: 'KeyA', shiftKey: true })),
    ).toBe('shift+a')
  })

  it('returns null for bare modifier presses', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'Shift', code: 'ShiftLeft' }))).toBeNull()
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'Meta', code: 'MetaLeft' }))).toBeNull()
  })

  it('returns null for a plain unmodified key', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'a' }))).toBeNull()
  })

  it('returns "disabled" for Backspace and Delete', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'Backspace', code: 'Backspace' }))).toBe(
      'disabled',
    )
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'Delete', code: 'Delete' }))).toBe('disabled')
  })

  it('captures standalone function keys and Scroll Lock', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'F5', code: 'F5' }))).toBe('f5')
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'F24', code: 'F24' }))).toBe('f24')
    expect(
      hotkeyFromKeyboardEvent(keyEvent({ key: 'ScrollLock', code: 'ScrollLock' })),
    ).toBe('scrolllock')
  })

  it('uses the platform meta label for the meta modifier', () => {
    expect(
      hotkeyFromKeyboardEvent(keyEvent({ key: 'k', code: 'KeyK', metaKey: true }), 'command'),
    ).toBe('command+k')
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'k', code: 'KeyK', metaKey: true }), 'win')).toBe(
      'win+k',
    )
  })

  it('normalizes named keys through their codes', () => {
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'ArrowLeft', code: 'ArrowLeft' }))).toBeNull()
    expect(
      hotkeyFromKeyboardEvent(keyEvent({ key: 'ArrowLeft', code: 'ArrowLeft', altKey: true })),
    ).toBe('alt+left')
    expect(hotkeyFromKeyboardEvent(keyEvent({ key: 'Enter', code: 'Enter', ctrlKey: true }))).toBe(
      'ctrl+enter',
    )
  })
})

describe('metaKeyLabelForPlatform', () => {
  it('maps platforms to their meta label', () => {
    expect(metaKeyLabelForPlatform('MacIntel')).toBe('command')
    expect(metaKeyLabelForPlatform('darwin')).toBe('command')
    expect(metaKeyLabelForPlatform('Win32')).toBe('win')
    expect(metaKeyLabelForPlatform('Linux x86_64')).toBe('meta')
  })
})

describe('formatEdgeSwitchHotkeyForDisplay', () => {
  it('lowercases and strips whitespace', () => {
    expect(formatEdgeSwitchHotkeyForDisplay(' Alt + Shift + K ', 'win')).toBe('alt+shift+k')
  })

  it('rewrites every meta alias to the platform label', () => {
    expect(formatEdgeSwitchHotkeyForDisplay('cmd+k', 'command')).toBe('command+k')
    expect(formatEdgeSwitchHotkeyForDisplay('win+k', 'command')).toBe('command+k')
    expect(formatEdgeSwitchHotkeyForDisplay('meta+k', 'win')).toBe('win+k')
    expect(formatEdgeSwitchHotkeyForDisplay('super+k', 'meta')).toBe('meta+k')
  })

  it('keeps non-meta parts untouched', () => {
    expect(formatEdgeSwitchHotkeyForDisplay('ctrl+alt+f3', 'win')).toBe('ctrl+alt+f3')
  })
})
