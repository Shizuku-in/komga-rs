import {pollJsonArray, pollPageContent, signInAt} from './helpers'
import {expect, test} from './test'

test('library creation, scan, analysis and browsing work in a browser', async ({page, server}) => {
  test.setTimeout(120_000)
  await signInAt(page, server.url)
  await expect(page.getByRole('heading', {name: 'Welcome to Komga'})).toBeVisible()

  await page.getByRole('button', {name: 'Add library'}).click()
  const libraryDialog = page.getByRole('dialog').filter({hasText: 'Add Library'})
  await libraryDialog.getByLabel('Name').fill('E2E Library')
  await libraryDialog.getByLabel('Root folder').fill(server.libraryRoot)
  for (let step = 0; step < 3; step += 1) {
    await libraryDialog.getByRole('button', {name: 'Next', exact: true}).click()
  }
  await libraryDialog.getByRole('button', {name: 'Add', exact: true}).click()
  await expect(libraryDialog).not.toHaveClass(/v-dialog__content--active/)

  const libraries = await pollJsonArray(page, `${server.url}/api/v1/libraries`, value => value.length === 1)
  const libraryId = libraries[0].id as string
  const series = await pollPageContent(page, `${server.url}/api/v1/series?library_id=${libraryId}&unpaged=true`, 1)
  const seriesId = series[0].id as string
  await pollPageContent(
    page,
    `${server.url}/api/v1/books?series_id=${seriesId}&unpaged=true`,
    1,
    book => book.media.status === 'READY',
  )

  await page.goto(`${server.url}/libraries/${libraryId}/series`)
  await expect(page.getByRole('link', {name: 'Example Series', exact: true}).first()).toBeVisible()
  await page.locator('.v-toolbar .mdi-dots-vertical').first().click()
  await page.getByText('Analyze', {exact: true}).last().click()
  const analyzeDialog = page.getByRole('dialog').filter({hasText: 'Analyze library'})
  await analyzeDialog.getByRole('button', {name: 'Analyze', exact: true}).click()
  await expect(analyzeDialog).not.toHaveClass(/v-dialog__content--active/)

  await page.getByRole('link', {name: 'Example Series', exact: true}).first().click()
  await expect(page).toHaveURL(new RegExp(`/series/${seriesId}$`))
})
