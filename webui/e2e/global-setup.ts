import {spawn, spawnSync} from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {stripVTControlCharacters} from 'node:util'

import {createFixtures} from './fixtures'
import {E2eState, statePath} from './state'

const SERVER_READY_TIMEOUT_MS = 30_000
const SERVER_READY_REQUEST_TIMEOUT_MS = 2_000
const SERVER_READY_RESPONSE_LIMIT = 1_024

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
    const oauth = await startOAuthProvider(path.join(temporaryRoot, 'oauth-provider.log'))
    processes.push(oauth.processId)
    const root = await startServer(binary, path.join(temporaryRoot, 'root-server'), '', oauth.url)
    processes.push(root.processId)
    const context = await startServer(binary, path.join(temporaryRoot, 'context-server'), '/komga', oauth.url)
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

async function startOAuthProvider(logPath: string): Promise<{processId: number, url: string}> {
  const log = fs.openSync(logPath, 'a')
  const child = spawn(process.execPath, [path.join(__dirname, 'fake-oauth-provider.cjs')], {stdio: ['ignore', log, log]})
  fs.closeSync(log)
  if (child.pid === undefined) throw new Error('OAuth provider process did not receive a PID')
  child.unref()

  try {
    const deadline = Date.now() + SERVER_READY_TIMEOUT_MS
    while (Date.now() < deadline) {
      if (child.exitCode !== null) {
        throw new Error(`OAuth provider exited with status ${child.exitCode}: ${fs.readFileSync(logPath, 'utf8')}`)
      }
      const output = fs.readFileSync(logPath, 'utf8')
      const match = stripVTControlCharacters(output).match(/listening address=127\.0\.0\.1:(\d+)/)
      if (match) return {processId: child.pid, url: `http://127.0.0.1:${match[1]}`}
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    throw new Error(`OAuth provider did not become ready:\n${fs.readFileSync(logPath, 'utf8')}`)
  } catch (error) {
    stopProcess(child.pid)
    throw error
  }
}

async function startServer(
  binary: string,
  configDirectory: string,
  contextPath: string,
  oauthProviderUrl: string,
): Promise<{processId: number, url: string}> {
  fs.mkdirSync(configDirectory, {recursive: true})
  fs.writeFileSync(path.join(configDirectory, 'oauth2.json'), JSON.stringify({
    accountCreation: false,
    providers: [{
      type: 'oauth2',
      registrationId: 'github',
      name: 'E2E OAuth',
      clientId: 'komga-e2e',
      clientAuthenticationMethod: 'none',
      authorizationUri: `${oauthProviderUrl}/authorize`,
      tokenUri: `${oauthProviderUrl}/token`,
      userInfoUri: `${oauthProviderUrl}/user`,
      emailAttribute: 'email',
      scopes: ['profile', 'email'],
    }],
  }, null, 2))
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
    let lastReadyCheck = 'server has not logged a listening address'
    while (Date.now() < deadline) {
      if (child.exitCode !== null) {
        throw new Error(`server exited with status ${child.exitCode}: ${fs.readFileSync(logPath, 'utf8')}`)
      }
      const output = fs.readFileSync(logPath, 'utf8')
      const match = stripVTControlCharacters(output).match(/listening address=127\.0\.0\.1:(\d+)/)
      if (match) {
        const origin = `http://127.0.0.1:${match[1]}`
        const url = `${origin}${contextPath}`
        const healthUrl = `${url}/actuator/health`
        const controller = new AbortController()
        const requestTimeout = setTimeout(() => controller.abort(), SERVER_READY_REQUEST_TIMEOUT_MS)
        try {
          const response = await fetch(healthUrl, {signal: controller.signal})
          if (response.ok) return {processId: child.pid, url}
          const responseBody = summarizeResponse(await response.text())
          lastReadyCheck = `${healthUrl} returned ${response.status} ${response.statusText}${responseBody ? `: ${responseBody}` : ''}`
        } catch (error) {
          lastReadyCheck = `${healthUrl} failed: ${formatError(error)}`
        } finally {
          clearTimeout(requestTimeout)
        }
      }
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    throw new Error([
      `server did not become ready within ${SERVER_READY_TIMEOUT_MS}ms`,
      `last readiness check: ${lastReadyCheck}`,
      `server log:\n${fs.readFileSync(logPath, 'utf8')}`,
    ].join('\n'))
  } catch (error) {
    stopProcess(child.pid)
    throw error
  }
}

function summarizeResponse(body: string): string {
  const summary = body.replace(/\s+/g, ' ').trim()
  return summary.length > SERVER_READY_RESPONSE_LIMIT
    ? `${summary.slice(0, SERVER_READY_RESPONSE_LIMIT)}...`
    : summary
}

function formatError(error: unknown): string {
  return error instanceof Error ? `${error.name}: ${error.message}` : String(error)
}

function stopProcess(processId: number): void {
  try {
    process.kill(processId, 'SIGTERM')
  } catch {
    // The process may already have exited after a setup failure.
  }
}
