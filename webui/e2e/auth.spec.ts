import {claimThroughUi, signIn, signInWithOAuth} from './helpers'
import {expect, test} from './test'

test.use({claimServer: false})

test('claim, password login and session refresh work in a browser', async ({page, server}) => {
  const nativeDialogs: string[] = []
  page.on('dialog', async dialog => {
    nativeDialogs.push(dialog.type())
    await dialog.dismiss()
  })

  await claimThroughUi(page, server.url)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
  expect(nativeDialogs).toEqual([])

  await page.getByText('Log Out', {exact: true}).click()
  await expect(page).toHaveURL(/\/login\?logout=true/)
  await signIn(page)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()
})

test('OAuth login is recorded in authentication activity', async ({page, server}) => {
  await claimThroughUi(page, server.url)
  await page.getByText('Log Out', {exact: true}).click()
  await signInWithOAuth(page)

  await page.goto(`${server.url}/account/authentication-activity`)
  await expect(page.getByText('OAuth2:E2E OAuth', {exact: true})).toBeVisible()
})
