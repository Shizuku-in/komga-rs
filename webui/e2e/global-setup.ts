import {spawn, spawnSync} from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {createFixtures} from './fixtures'
import {E2eState, statePath} from './state'

const SERVER_READY_TIMEOUT_MS = 30_000

export default async function globalSetup(): Promise<void> {
  const webuiRoot = path.resolve(__dirname, '..')
  const repositoryRoot = path.resolve(webuiRoot, '..')
  const dist = path.join(webuiRoot, 'dist')
  if (!fs.existsSync(path.join(dist, 'index.html'))) {
    throw new Error('webui/dist is missing; run npm run build before npm run test:e2e')
  }
  const build = spawnSync('cargo', ['build', '--locked', '-p', 'komga-server'], {
    cwd: repositoryRoot,
    env: {...process.env, KOMGA_WEBUI_DIST_DIR: dist},
    stdio: 'inherit',
  })
  if (build.status !== 0) {
    throw new Error(`cargo build failed with status ${build.status}`)
  }

  const temporaryRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'komga-rs-e2e-'))
  const fixtures = createFixtures(temporaryRoot)
  const binary = path.join(repositoryRoot, 'target', 'debug', process.platform === 'win32' ? 'komga-server.exe' : 'komga-server')
  const processes: number[] = []
  try {
    const root = await startServer(binary, path.join(temporaryRoot, 'root-server'), '')
    processes.push(root.processId)
    const context = await startServer(binary, path.join(temporaryRoot, 'context-server'), '/komga')
    processes.push(context.processId)
    const state: E2eState = {
      temporaryRoot,
      rootUrl: root.url,
      contextUrl: context.url,
      libraryRoot: fixtures.libraryRoot,
      importRoot: fixtures.importRoot,
      processIds: processes,
    }
    fs.writeFileSync(statePath, JSON.stringify(state, null, 2))
  } catch (error) {
    for (const processId of processes) stopProcess(processId)
    fs.rmSync(temporaryRoot, {recursive: true, force: true})
    throw error
  }
}

async function startServer(binary: string, configDirectory: string, contextPath: string): Promise<{processId: number, url: string}> {
  fs.mkdirSync(configDirectory, {recursive: true})
  const logPath = path.join(configDirectory, 'server.log')
  const log = fs.openSync(logPath, 'a')
  const args = ['--config-dir', configDirectory, '--host', '127.0.0.1', '--port', '0']
  if (contextPath) args.push('--context-path', contextPath)
  const child = spawn(binary, args, {stdio: ['ignore', log, log]})
  fs.closeSync(log)
  if (child.pid === undefined) throw new Error('server process did not receive a PID')
  child.unref()

  try {
    const deadline = Date.now() + SERVER_READY_TIMEOUT_MS
    while (Date.now() < deadline) {
      if (child.exitCode !== null) {
        throw new Error(`server exited with status ${child.exitCode}: ${fs.readFileSync(logPath, 'utf8')}`)
      }
      const output = fs.readFileSync(logPath, 'utf8')
      const match = output.match(/listening address=127\.0\.0\.1:(\d+)/)
      if (match) {
        const origin = `http://127.0.0.1:${match[1]}`
        const url = `${origin}${contextPath}`
        try {
          const response = await fetch(`${url}/actuator/health`)
          if (response.ok) return {processId: child.pid, url}
        } catch {
          // The listener can be logged before it accepts the first request.
        }
      }
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    throw new Error(`server did not become ready: ${fs.readFileSync(logPath, 'utf8')}`)
  } catch (error) {
    stopProcess(child.pid)
    throw error
  }
}

function stopProcess(processId: number): void {
  try {
    process.kill(processId, 'SIGTERM')
  } catch {
    // The process may already have exited after a setup failure.
  }
}
