import {spawn} from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import {stripVTControlCharacters} from 'node:util'

const SERVER_READY_TIMEOUT_MS = 30_000
const SERVER_READY_REQUEST_TIMEOUT_MS = 2_000
const SERVER_READY_RESPONSE_LIMIT = 1_024

export interface StartedProcess {
  processId: number
  url: string
}

export async function startOAuthProvider(logPath: string): Promise<StartedProcess> {
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
      await wait(100)
    }
    throw new Error(`OAuth provider did not become ready:\n${fs.readFileSync(logPath, 'utf8')}`)
  } catch (error) {
    stopProcess(child.pid)
    throw error
  }
}

export async function startServer(
  binary: string,
  configDirectory: string,
  contextPath: string,
  oauthProviderUrl: string,
): Promise<StartedProcess> {
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
      await wait(100)
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

export function stopProcess(processId: number, signal: NodeJS.Signals = 'SIGTERM'): void {
  try {
    process.kill(processId, signal)
  } catch {
    // The process may already have exited.
  }
}

export async function stopProcessAndWait(processId: number): Promise<void> {
  stopProcess(processId)
  if (await waitForExit(processId, 10_000)) return
  stopProcess(processId, 'SIGKILL')
  await waitForExit(processId, 2_000)
}

async function waitForExit(processId: number, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      process.kill(processId, 0)
      await wait(100)
    } catch {
      return true
    }
  }
  return false
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

function wait(milliseconds: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, milliseconds))
}
