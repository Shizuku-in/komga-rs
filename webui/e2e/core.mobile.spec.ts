import {pollPageContent, signInAt} from './helpers'
import {expect, test} from './test'

test.use({seedLibrary: true})

test('mobile Chromium can log in, browse a library and open the reader', async ({page, server}) => {
  test.setTimeout(90_000)
  await signInAt(page, server.url)
  const series = await pollPageContent(
    page,
    `${server.url}/api/v1/series?library_id=${server.libraryId}&unpaged=true`,
    1,
  )
  const books = await pollPageContent(
    page,
    `${server.url}/api/v1/books?series_id=${series[0].id}&unpaged=true`,
    1,
    book => book.media.status === 'READY',
  )

  await page.goto(`${server.url}/libraries/${server.libraryId}/series`)
  await expect(page.getByRole('link', {name: 'Example Series', exact: true}).first()).toBeVisible()
  await page.getByRole('link', {name: 'Example Series', exact: true}).first().click()
  await expect(page).toHaveURL(new RegExp(`/series/${series[0].id}$`))

  await page.goto(`${server.url}/book/${books[0].id}`)
  await page.getByRole('link', {name: 'Read', exact: true}).first().click()
  await expect(page.getByAltText('Page 1')).toBeVisible()
})
