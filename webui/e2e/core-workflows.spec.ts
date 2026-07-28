import fs from 'node:fs'
import path from 'node:path'

import {APIResponse, Page, expect, test} from '@playwright/test'

import {readState} from './state'

const ADMIN_EMAIL = 'admin@example.org'
const ADMIN_PASSWORD = 'password123'

test.describe.configure({mode: 'serial'})

const browserProblems = new WeakMap<Page, string[]>()

test.beforeEach(async ({page}) => {
  const problems: string[] = []
  browserProblems.set(page, problems)
  page.on('console', message => {
    const failedResource = message.text().startsWith('Failed to load resource:')
    if ((message.type() === 'error' && !failedResource) || message.type() === 'warning') {
      problems.push(`console.${message.type()}: ${message.text()}`)
    }
  })
  page.on('pageerror', error => problems.push(`pageerror: ${error.message}`))
  page.on('response', response => {
    const resourceType = response.request().resourceType()
    if (response.status() >= 400 && ['document', 'script', 'stylesheet'].includes(resourceType)) {
      problems.push(`${resourceType} ${response.status()}: ${response.url()}`)
    }
  })
})

test.afterEach(async ({page}) => {
  expect(browserProblems.get(page)).toEqual([])
})

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
  await page.getByRole('textbox', {name: 'Import from folder', exact: true}).fill(state.importRoot)
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

test('search, administration routes and responsive navigation render without browser errors', async ({page}, testInfo) => {
  test.setTimeout(120_000)
  const state = readState()

  await page.goto(state.rootUrl)
  await signIn(page)

  const libraries = await pollJsonArray(page, `${state.rootUrl}/api/v1/libraries`, value => value.length === 1)
  const libraryId = libraries[0].id as string

  const search = page.getByPlaceholder('Search')
  await search.fill('Example')
  const searchMenu = page.locator('.v-autocomplete__content')
  await expect(searchMenu.getByText('Example Series', {exact: true})).toBeVisible()
  await searchMenu.getByText('Example Series', {exact: true}).click()
  await expect(page).toHaveURL(/\/series\/[^/]+$/)

  await page.locator('.v-toolbar .mdi-pencil').click()
  const editSeriesDialog = page.getByRole('dialog').filter({hasText: 'Edit Example Series'})
  await expect(editSeriesDialog).toBeVisible()
  await editSeriesDialog.getByRole('tab', {name: 'Links', exact: true}).click()
  await editSeriesDialog.locator('.mdi-plus').click()
  await editSeriesDialog.getByLabel('Label').fill('Project')
  await editSeriesDialog.getByLabel('URL').fill('not-a-url')
  await editSeriesDialog.getByRole('button', {name: 'Save changes', exact: true}).last().click()
  await expect(editSeriesDialog.getByText('Must be a valid URL', {exact: true})).toBeVisible()
  await expect(editSeriesDialog).toBeVisible()
  await editSeriesDialog.getByRole('button', {name: 'Cancel', exact: true}).last().click()
  await expect(editSeriesDialog).not.toBeVisible()

  const routes = [
    `/libraries/${libraryId}/series`,
    '/settings/users',
    '/settings/server',
    '/media-management/analysis',
    '/media-management/duplicate-pages/known',
    '/history',
    '/account/api-keys',
    '/import/readlist',
  ]
  for (const route of routes) {
    await page.goto(`${state.rootUrl}${route}`)
    await expect(page.getByRole('main')).toBeVisible()
    await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
    expect(await hasHorizontalOverflow(page)).toBe(false)
  }

  await page.goto(`${state.rootUrl}/settings/server`)
  await page.screenshot({path: testInfo.outputPath('desktop-settings.png'), fullPage: true})

  await page.setViewportSize({width: 390, height: 844})
  await page.goto(`${state.rootUrl}/libraries/${libraryId}/series`)
  await page.locator('.v-app-bar-nav-icon').click()
  const navigationDrawer = page.locator('.v-navigation-drawer--active').filter({hasText: 'E2E Library'})
  await expect(navigationDrawer).toBeVisible()
  await expect(navigationDrawer.getByText('E2E Library', {exact: true})).toBeVisible()
  await expect.poll(async () => (await navigationDrawer.boundingBox())?.width ?? 0).toBeGreaterThan(200)
  await expect.poll(async () => (await navigationDrawer.boundingBox())?.x ?? -1).toBeGreaterThanOrEqual(0)
  expect(await hasHorizontalOverflow(page)).toBe(false)
  await page.screenshot({path: testInfo.outputPath('mobile-navigation.png'), fullPage: true})
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

async function hasHorizontalOverflow(page: Page): Promise<boolean> {
  return page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1)
}
