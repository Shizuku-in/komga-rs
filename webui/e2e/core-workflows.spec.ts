import fs from 'node:fs'
import path from 'node:path'

import {APIResponse, Page, expect, test} from '@playwright/test'

import {readState} from './state'

const ADMIN_EMAIL = 'admin@example.org'
const ADMIN_PASSWORD = 'password123'

test.describe.configure({mode: 'serial'})

test('claim, login, library reading and COPY import work in a browser', async ({page}) => {
  test.setTimeout(180_000)
  const state = readState()
  const nativeDialogs: string[] = []
  page.on('dialog', async dialog => {
    nativeDialogs.push(dialog.type())
    await dialog.dismiss()
  })

  await claimThroughUi(page, state.rootUrl)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
  expect(nativeDialogs).toEqual([])

  await page.getByText('Log Out', {exact: true}).click()
  await expect(page).toHaveURL(/\/login\?logout=true/)
  await signIn(page)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()

  await page.getByText('Log Out', {exact: true}).click()
  await signInWithOAuth(page)
  await page.goto(`${state.rootUrl}/account/authentication-activity`)
  await expect(page.getByText('OAuth2:E2E OAuth', {exact: true})).toBeVisible()
  await page.goto(state.rootUrl)

  await page.getByRole('button', {name: 'Add library'}).click()
  const libraryDialog = page.getByRole('dialog').filter({hasText: 'Add Library'})
  await libraryDialog.getByLabel('Name').fill('E2E Library')
  await libraryDialog.getByLabel('Root folder').fill(state.libraryRoot)
  for (let step = 0; step < 3; step++) {
    await libraryDialog.getByRole('button', {name: 'Next', exact: true}).click()
  }
  await libraryDialog.getByRole('button', {name: 'Add', exact: true}).click()
  await expect(libraryDialog).not.toHaveClass(/v-dialog__content--active/)

  const libraries = await pollJsonArray(page, `${state.rootUrl}/api/v1/libraries`, value => value.length === 1)
  const libraryId = libraries[0].id as string
  const series = await pollPageContent(page, `${state.rootUrl}/api/v1/series?library_id=${libraryId}&unpaged=true`, 1)
  const seriesId = series[0].id as string
  let books = await pollPageContent(page, `${state.rootUrl}/api/v1/books?series_id=${seriesId}&unpaged=true`, 1, book => book.media.status === 'READY')
  const bookId = books[0].id as string

  await page.goto(`${state.rootUrl}/libraries/${libraryId}/series`)
  await expect(page.getByRole('link', {name: 'Example Series', exact: true}).first()).toBeVisible()
  await page.locator('.v-toolbar .mdi-dots-vertical').first().click()
  await page.getByText('Analyze', {exact: true}).last().click()
  const analyzeDialog = page.getByRole('dialog').filter({hasText: 'Analyze library'})
  await analyzeDialog.getByRole('button', {name: 'Analyze', exact: true}).click()
  await expect(analyzeDialog).not.toHaveClass(/v-dialog__content--active/)

  await page.getByRole('link', {name: 'Example Series', exact: true}).first().click()
  await expect(page).toHaveURL(new RegExp(`/series/${seriesId}$`))
  await page.locator(`a[href="/book/${bookId}"]`).click()
  await expect(page).toHaveURL(new RegExp(`/book/${bookId}$`))
  await page.getByRole('link', {name: 'Read', exact: true}).first().click()
  await expect(page).toHaveURL(new RegExp(`/book/${bookId}/read`))
  await page.locator('.right-quarter').click()
  await expect.poll(async () => {
    const book = await responseJson(await page.request.get(`${state.rootUrl}/api/v1/books/${bookId}`))
    return book.readProgress?.page
  }, {timeout: 20_000}).toBe(2)

  await page.goto(`${state.rootUrl}/import/books`)
  await page.getByLabel('Import from folder').fill(state.importRoot)
  await page.getByRole('button', {name: 'Scan', exact: true}).click()
  await expect(page.getByText('Import Candidate', {exact: true}).first()).toBeVisible()
  await expect(page.getByText('Example Series', {exact: true}).last()).toBeVisible({timeout: 20_000})

  let interceptedCopy = false
  await page.route('**/api/v1/books/import', async route => {
    const payload = route.request().postDataJSON()
    payload.copyMode = 'COPY'
    interceptedCopy = true
    const headers = {...route.request().headers(), 'content-type': 'application/json'}
    delete headers['content-length']
    await route.continue({headers, postData: JSON.stringify(payload)})
  })
  const importButton = page.getByRole('main').getByRole('button', {name: 'Import', exact: true})
  await expect(importButton).toBeEnabled({timeout: 20_000})
  await importButton.click()
  await expect.poll(() => interceptedCopy).toBe(true)

  books = await pollPageContent(page, `${state.rootUrl}/api/v1/books?series_id=${seriesId}&unpaged=true`, 2)
  expect(books.map(book => book.name).sort()).toEqual(['Import Candidate', 'Volume 1'])
  expect(fs.existsSync(path.join(state.importRoot, 'Import Candidate.cbz'))).toBe(true)
  expect(fs.existsSync(path.join(state.libraryRoot, 'Example Series', 'Import Candidate.cbz'))).toBe(true)
})

test('context-path login, deep-link refresh and static assets stay under /komga', async ({page}) => {
  test.setTimeout(60_000)
  const state = readState()
  const assets: string[] = []
  page.on('response', response => {
    if (/\.(?:css|js)(?:\?|$)/.test(response.url()) && response.ok()) assets.push(response.url())
  })

  await claimThroughUi(page, state.contextUrl)
  await page.getByText('Log Out', {exact: true}).click()
  await signInWithOAuth(page)
  await page.goto(`${state.contextUrl}/settings/server`)
  await expect(page.getByText('Server Settings', {exact: true})).toBeVisible()
  await page.reload()
  await expect(page).toHaveURL(`${state.contextUrl}/settings/server`)
  await expect(page.getByText('Server Settings', {exact: true})).toBeVisible()
  expect(assets.some(url => new URL(url).pathname.startsWith('/komga/js/'))).toBe(true)
  expect(assets.some(url => new URL(url).pathname.startsWith('/komga/css/'))).toBe(true)
})

async function claimThroughUi(page: Page, baseUrl: string): Promise<void> {
  const response = await page.goto(baseUrl)
  expect(response?.status()).toBe(200)
  await expect(page.getByRole('button', {name: 'Create user account'})).toBeEnabled()
  await page.getByLabel('Email').fill(ADMIN_EMAIL)
  await page.getByLabel('Password').fill(ADMIN_PASSWORD)
  await page.getByRole('button', {name: 'Create user account'}).click()
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
}

async function signIn(page: Page): Promise<void> {
  await page.getByLabel('Email').fill(ADMIN_EMAIL)
  await page.getByLabel('Password').fill(ADMIN_PASSWORD)
  await page.getByRole('button', {name: 'Sign in', exact: true}).click()
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
}

async function signInWithOAuth(page: Page): Promise<void> {
  const popupPromise = page.waitForEvent('popup')
  await page.getByRole('button', {name: 'Sign in with E2E OAuth'}).click()
  const popup = await popupPromise
  if (!popup.isClosed()) await popup.waitForEvent('close')
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
}

async function pollJsonArray(
  page: Page,
  url: string,
  ready: (value: Array<Record<string, any>>) => boolean,
): Promise<Array<Record<string, any>>> {
  let result: Array<Record<string, any>> = []
  await expect.poll(async () => {
    result = await responseJson(await page.request.get(url))
    return ready(result)
  }, {timeout: 30_000}).toBe(true)
  return result
}

async function pollPageContent(
  page: Page,
  url: string,
  count: number,
  itemReady: (value: Record<string, any>) => boolean = () => true,
): Promise<Array<Record<string, any>>> {
  let result: Array<Record<string, any>> = []
  await expect.poll(async () => {
    const body = await responseJson(await page.request.get(url))
    result = body.content
    return result.length === count && result.every(itemReady)
  }, {timeout: 40_000}).toBe(true)
  return result
}

async function responseJson(response: APIResponse): Promise<any> {
  if (!response.ok()) {
    throw new Error(`${response.status()} ${response.url()}: ${await response.text()}`)
  }
  return response.json()
}
