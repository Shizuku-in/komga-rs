import fs from 'node:fs'
import path from 'node:path'

export interface E2eState {
  temporaryRoot: string
  rootUrl: string
  contextUrl: string
  libraryRoot: string
  importRoot: string
  processIds: number[]
}

export const statePath = path.join(__dirname, '.state.json')

export function readState(): E2eState {
  return JSON.parse(fs.readFileSync(statePath, 'utf8')) as E2eState
}
