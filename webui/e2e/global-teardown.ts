import fs from 'node:fs'
import path from 'node:path'

import {stopProcessAndWait} from './processes'
import {readState, statePath} from './state'

export default async function globalTeardown(): Promise<void> {
  if (!fs.existsSync(statePath)) return
  const state = readState()
  await Promise.all(state.processIds.map(stopProcessAndWait))

  if (fs.existsSync(state.oauthLogPath)) {
    const outputDirectory = path.resolve(__dirname, '..', 'test-results', 'global')
    fs.mkdirSync(outputDirectory, {recursive: true})
    fs.copyFileSync(state.oauthLogPath, path.join(outputDirectory, 'oauth-provider.log'))
  }

  fs.rmSync(state.temporaryRoot, {recursive: true, force: true})
  fs.rmSync(statePath, {force: true})
}
