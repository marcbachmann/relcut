'use strict'
// Installs relcut, then becomes relcut through execve: no node process stays behind whose environment still holds the
// tokens, and no step prints the settings as an env block.
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const {spawnSync} = require('node:child_process')

const OWN = ['COMMAND', 'VERSION']
const MARKERS = ['TOKEN', 'SECRET', 'PASSWORD', '_AUTH']

function print(line) {
  fs.writeSync(1, `${line}\n`)
}

function fail(message) {
  print(`::error::relcut: ${message}`)
  process.exit(1)
}

function input(name) {
  return (process.env[`INPUT_${name}`] || '').trim()
}

// What the installers see: neither the inputs nor anything credential-like.
function untrusted() {
  const env = {}
  for (const [key, value] of Object.entries(process.env)) {
    if (key.startsWith('INPUT_') || MARKERS.some((m) => key.toUpperCase().includes(m))) continue
    env[key] = value
  }
  return env
}

function readOutputs(file) {
  const outputs = {}
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    const eq = line.indexOf('=')
    if (eq > 0) outputs[line.slice(0, eq)] = line.slice(eq + 1)
  }
  return outputs
}

function installRelcut(root, temp) {
  const outputs = path.join(temp, 'relcut-setup-outputs')
  fs.writeFileSync(outputs, '')
  const run = spawnSync('bash', [path.join(root, 'setup/install.sh')], {
    encoding: 'utf8',
    env: {
      ...untrusted(),
      GITHUB_ACTION_PATH: root,
      GITHUB_OUTPUT: outputs,
      RELCUT_REPOSITORY: process.env.GITHUB_ACTION_REPOSITORY || '',
      RELCUT_REF: process.env.GITHUB_ACTION_REF || '',
      RELCUT_VERSION_WANTED: input('VERSION'),
    },
  })
  const said = `${run.stdout || ''}${run.stderr || ''}`.trim()
  if (run.status !== 0) {
    print(said)
    process.exit(run.status || 1)
  }
  const {path: bin, version} = readOutputs(outputs)
  if (!bin) fail('setup named no binary')
  return {bin, version}
}

function main() {
  if (typeof process.execve !== 'function') fail(`node ${process.version} has no process.execve; the action needs node 24`)
  const root = path.resolve(__dirname, '..')
  const temp = process.env.RUNNER_TEMP || os.tmpdir()
  const command = input('COMMAND') || 'release'
  const relcut = installRelcut(root, temp)
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, `relcut-version=${relcut.version}\n`)

  const env = {}
  for (const [key, value] of Object.entries(process.env)) {
    if (!key.startsWith('INPUT_')) env[key] = value
  }
  for (const key of Object.keys(process.env)) {
    const name = key.startsWith('INPUT_') && key.slice('INPUT_'.length)
    if (!name || OWN.includes(name)) continue
    const setting = `RELCUT_${name.replace(/-/g, '_')}`
    const value = input(name)
    if (value) env[setting] = value
    else delete env[setting]
  }
  process.execve(relcut.bin, [relcut.bin, command], env)
}

main()
