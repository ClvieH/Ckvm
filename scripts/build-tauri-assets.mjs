import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'

const root = dirname(dirname(fileURLToPath(import.meta.url)))

function run(command, args) {
  const resolvedCommand =
    process.platform === 'win32' && command === 'npm'
      ? 'cmd.exe'
      : process.platform === 'win32' && command === 'cargo'
        ? 'cargo.exe'
        : command
  const resolvedArgs =
    process.platform === 'win32' && command === 'npm'
      ? ['/d', '/s', '/c', ['npm', ...args].join(' ')]
      : args
  const result = spawnSync(resolvedCommand, resolvedArgs, {
    cwd: root,
    stdio: 'inherit',
  })

  if (result.error) {
    console.error(result.error.message)
    process.exit(1)
  }

  if (result.status !== 0) {
    process.exit(result.status ?? 1)
  }
}

run('npm', ['run', 'build'])

if (process.platform !== 'win32') {
  process.exit(0)
}

const target = 'x86_64-pc-windows-msvc'
run('cargo', [
  'build',
  '--manifest-path',
  'src-tauri/Cargo.toml',
  '-p',
  'mykvm-input-helper',
  '--release',
  '--target',
  target,
])

// Honor CARGO_TARGET_DIR (set by scripts/run-tauri-dev.ps1 / CI caches) so the
// sidecar is copied from wherever cargo actually placed it.
const cargoTargetRoot = process.env.CARGO_TARGET_DIR || join(root, 'src-tauri', 'target')
const source = join(
  cargoTargetRoot,
  target,
  'release',
  'mykvm-input-helper.exe',
)
const fallbackSource = join(
  cargoTargetRoot,
  'release',
  'mykvm-input-helper.exe',
)
const destination = join(
  root,
  'src-tauri',
  'binaries',
  `mykvm-input-helper-${target}.exe`,
)

mkdirSync(dirname(destination), { recursive: true })
copyFileSync(existsSync(source) ? source : fallbackSource, destination)
