import {APIResponse, Page, expect} from '@playwright/test'

import {ADMIN_EMAIL, ADMIN_PASSWORD} from './test'

export async function claimThroughUi(page: Page, baseUrl: string): Promise<void> {
  const response = await page.goto(baseUrl)
  expect(response?.status()).toBe(200)
  await expect(page.getByRole('button', {name: 'Create user account'})).toBeEnabled()
  await page.getByLabel('Email').fill(ADMIN_EMAIL)
  await page.getByLabel('Password').fill(ADMIN_PASSWORD)
  await page.getByRole('button', {name: 'Create user account'}).click()
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
}

export async function signInAt(page: Page, baseUrl: string): Promise<void> {
  await page.goto(baseUrl)
  await expect(page.getByRole('button', {name: 'Sign in', exact: true})).toBeEnabled()
  await signIn(page)
}

export async function signIn(page: Page): Promise<void> {
  await page.getByLabel('Email').fill(ADMIN_EMAIL)
  await page.getByLabel('Password').fill(ADMIN_PASSWORD)
  await page.getByRole('button', {name: 'Sign in', exact: true}).click()
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
}

export async function signInWithOAuth(page: Page): Promise<void> {
  const popupPromise = page.waitForEvent('popup')
  await page.getByRole('button', {name: 'Sign in with E2E OAuth'}).click()
  const popup = await popupPromise
  if (!popup.isClosed()) await popup.waitForEvent('close')
  await expect(page).not.toHaveURL(/\/login(?:\?|$)/)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
}

export async function pollJsonArray(
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

export async function pollPageContent(
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

export async function responseJson(response: APIResponse): Promise<any> {
  if (!response.ok()) {
    throw new Error(`${response.status()} ${response.url()}: ${await response.text()}`)
  }
  return response.json()
}
