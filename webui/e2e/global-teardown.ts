import fs from 'node:fs'

import {readState, statePath} from './state'

export default async function globalTeardown(): Promise<void> {
  if (!fs.existsSync(statePath)) return
  const state = readState()
  for (const processId of state.processIds) {
    try {
      process.kill(processId, 'SIGTERM')
    } catch {
      // A failed test can coincide with a server process exiting on its own.
    }
  }
  await Promise.all(state.processIds.map(waitForExit))
  fs.rmSync(state.temporaryRoot, {recursive: true, force: true})
  fs.rmSync(statePath, {force: true})
}

async function waitForExit(processId: number): Promise<void> {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    try {
      process.kill(processId, 0)
      await new Promise(resolve => setTimeout(resolve, 100))
    } catch {
      return
    }
  }
}
