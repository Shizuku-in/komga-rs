import {pollPageContent, responseJson, signInAt} from './helpers'
import {expect, test} from './test'

test.use({seedLibrary: true})

test('the fixed-layout reader renders real pages and restores progress', async ({page, server}) => {
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
  const bookId = books[0].id as string

  await page.goto(`${server.url}/book/${bookId}`)
  await page.getByRole('link', {name: 'Read', exact: true}).first().click()
  await expect(page).toHaveURL(new RegExp(`/book/${bookId}/read`))
  await expectRenderedPage(page, 1, 64, 48)

  await page.locator('.right-quarter').click()
  await expectRenderedPage(page, 2, 48, 64)
  await expect.poll(async () => {
    const book = await responseJson(await page.request.get(`${server.url}/api/v1/books/${bookId}`))
    return book.readProgress?.page
  }, {timeout: 20_000}).toBe(2)

  await page.reload()
  await expectRenderedPage(page, 2, 48, 64)
})

async function expectRenderedPage(
  page: import('@playwright/test').Page,
  number: number,
  width: number,
  height: number,
): Promise<void> {
  const image = page.getByAltText(`Page ${number}`)
  await expect(image).toBeVisible()
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => ({
    complete: element.complete,
    width: element.naturalWidth,
    height: element.naturalHeight,
  }))).toEqual({complete: true, width, height})
}
