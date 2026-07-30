import {spawnSync} from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {startOAuthProvider, stopProcessAndWait} from './processes'
import {E2eState, statePath} from './state'

export default async function globalSetup(): Promise<void> {
  const webuiRoot = path.resolve(__dirname, '..')
  const repositoryRoot = path.resolve(webuiRoot, '..')
  const dist = path.join(webuiRoot, 'dist')
  if (!fs.existsSync(path.join(dist, 'index.html'))) {
    throw new Error('webui/dist is missing; run bun run build before bun run test:e2e')
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
  const binary = path.join(repositoryRoot, 'target', 'debug', process.platform === 'win32' ? 'komga-server.exe' : 'komga-server')
  const oauthLogPath = path.join(temporaryRoot, 'oauth-provider.log')
  let oauthProcessId: number | undefined
  try {
    const oauth = await startOAuthProvider(oauthLogPath)
    oauthProcessId = oauth.processId
    const state: E2eState = {
      temporaryRoot,
      binary,
      oauthProviderUrl: oauth.url,
      oauthLogPath,
      processIds: [oauth.processId],
    }
    fs.writeFileSync(statePath, JSON.stringify(state, null, 2))
  } catch (error) {
    if (oauthProcessId !== undefined) await stopProcessAndWait(oauthProcessId)
    fs.rmSync(temporaryRoot, {recursive: true, force: true})
    throw error
  }
}
