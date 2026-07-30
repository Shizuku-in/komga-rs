import fs from 'node:fs'
import path from 'node:path'

import {Page, expect, test as base} from '@playwright/test'

import {createFixtures} from './fixtures'
import {startServer, stopProcessAndWait} from './processes'
import {readState} from './state'

export const ADMIN_EMAIL = 'admin@example.org'
export const ADMIN_PASSWORD = 'password123'

export interface E2eServer {
  url: string
  contextPath: string
  libraryRoot: string
  importRoot: string
  libraryId?: string
}

interface E2eOptions {
  contextPath: string
  claimServer: boolean
  seedLibrary: boolean
}

interface E2eFixtures {
  server: E2eServer
  browserDiagnostics: void
}

export const test = base.extend<E2eOptions & E2eFixtures>({
  contextPath: ['', {option: true}],
  claimServer: [true, {option: true}],
  seedLibrary: [false, {option: true}],

  server: async ({contextPath, claimServer, request, seedLibrary}, use, testInfo) => {
    const state = readState()
    const caseRoot = fs.mkdtempSync(path.join(state.temporaryRoot, 'case-'))
    const fixturePaths = createFixtures(caseRoot)
    const configDirectory = path.join(caseRoot, 'server')
    const logPath = path.join(configDirectory, 'server.log')
    let processId: number | undefined

    try {
      const started = await startServer(state.binary, configDirectory, contextPath, state.oauthProviderUrl)
      processId = started.processId
      let libraryId: string | undefined

      if (claimServer) {
        const claim = await request.post(`${started.url}/api/v1/claim`, {
          headers: {
            'X-Komga-Email': ADMIN_EMAIL,
            'X-Komga-Password': ADMIN_PASSWORD,
          },
        })
        await requireOk(claim, 'claim test server')
      }

      if (seedLibrary) {
        if (!claimServer) throw new Error('seedLibrary requires claimServer')
        const library = await request.post(`${started.url}/api/v1/libraries`, {
          headers: {
            authorization: basicAuthorization(),
          },
          data: {
            name: 'E2E Library',
            root: fixturePaths.libraryRoot,
            scanInterval: 'DISABLED',
          },
        })
        await requireOk(library, 'seed test library')
        libraryId = (await library.json()).id as string
      }

      await use({
        url: started.url,
        contextPath,
        libraryRoot: fixturePaths.libraryRoot,
        importRoot: fixturePaths.importRoot,
        libraryId,
      })
    } finally {
      if (processId !== undefined) await stopProcessAndWait(processId)
      if (fs.existsSync(logPath) && testInfo.status !== testInfo.expectedStatus) {
        await testInfo.attach('komga-server.log', {
          path: logPath,
          contentType: 'text/plain',
        })
      }
      fs.rmSync(caseRoot, {recursive: true, force: true})
    }
  },

  browserDiagnostics: [async ({context, page, server}, use, testInfo) => {
    // Keep the per-test server alive until browser diagnostics have been checked.
    void server
    const issues: string[] = []
    const monitoredPages = new WeakSet<Page>()

    const monitor = (candidate: Page) => {
      if (monitoredPages.has(candidate)) return
      monitoredPages.add(candidate)
      candidate.on('pageerror', error => issues.push(`page error: ${error.message}`))
      candidate.on('console', message => {
        if (message.type() === 'error' && !message.text().startsWith('Failed to load resource:')) {
          issues.push(`console error: ${message.text()}`)
        }
      })
      candidate.on('requestfailed', request => {
        const failure = request.failure()?.errorText ?? 'unknown error'
        const pathname = new URL(request.url()).pathname
        if (failure.includes('ERR_ABORTED') || pathname.endsWith('/sse/v1/events')) return
        issues.push(`request failed: ${request.method()} ${request.url()} (${failure})`)
      })
      candidate.on('response', response => {
        if (response.status() >= 500) {
          issues.push(`HTTP ${response.status()}: ${response.request().method()} ${response.url()}`)
        }
      })
    }

    monitor(page)
    context.on('page', monitor)
    await use()

    if (issues.length > 0) {
      await testInfo.attach('browser-diagnostics.txt', {
        body: Buffer.from(issues.join('\n')),
        contentType: 'text/plain',
      })
      if (testInfo.status === testInfo.expectedStatus) {
        expect(issues, 'unexpected browser errors').toEqual([])
      }
    }
  }, {auto: true}],
})

export {expect} from '@playwright/test'

function basicAuthorization(): string {
  return `Basic ${Buffer.from(`${ADMIN_EMAIL}:${ADMIN_PASSWORD}`).toString('base64')}`
}

async function requireOk(
  response: {ok(): boolean, status(): number, text(): Promise<string>},
  operation: string,
): Promise<void> {
  if (!response.ok()) {
    throw new Error(`could not ${operation}: ${response.status()} ${await response.text()}`)
  }
}
