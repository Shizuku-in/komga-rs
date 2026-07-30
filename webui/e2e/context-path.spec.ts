import {claimThroughUi, pollPageContent, responseJson, signInWithOAuth} from './helpers'
import {expect, test} from './test'

test.use({claimServer: false, contextPath: '/komga'})

test('context-path browser traffic, deep links and static assets stay under /komga', async ({page, server}) => {
  test.setTimeout(120_000)
  const assets: string[] = []
  const requestPaths: string[] = []
  const escapedRequests: string[] = []
  const serverOrigin = new URL(server.url).origin
  page.on('response', response => {
    const url = new URL(response.url())
    if (url.origin !== serverOrigin || !response.ok()) return
    requestPaths.push(url.pathname)
    if (/\.(?:css|js)(?:\?|$)/.test(response.url())) assets.push(response.url())
    if (/\/(?:api|sse|js|css)\//.test(url.pathname) && !url.pathname.startsWith('/komga/')) {
      escapedRequests.push(url.pathname)
    }
  })

  await claimThroughUi(page, server.url)
  await page.getByText('Log Out', {exact: true}).click()
  await signInWithOAuth(page)

  const libraryResponse = await page.request.post(`${server.url}/api/v1/libraries`, {
    data: {
      name: 'Context Library',
      root: server.libraryRoot,
      scanInterval: 'DISABLED',
    },
  })
  const library = await responseJson(libraryResponse)
  const series = await pollPageContent(
    page,
    `${server.url}/api/v1/series?library_id=${library.id}&unpaged=true`,
    1,
  )
  const books = await pollPageContent(
    page,
    `${server.url}/api/v1/books?series_id=${series[0].id}&unpaged=true`,
    1,
    book => book.media.status === 'READY',
  )

  await page.goto(`${server.url}/settings/server`)
  await expect(page.getByText('Server Settings', {exact: true})).toBeVisible()
  await page.reload()
  await expect(page).toHaveURL(`${server.url}/settings/server`)
  await expect(page.getByText('Server Settings', {exact: true})).toBeVisible()

  await page.goto(`${server.url}/book/${books[0].id}`)
  await page.getByRole('link', {name: 'Read', exact: true}).first().click()
  await expect(page.getByAltText('Page 1')).toBeVisible()
  await expect.poll(() => page.getByAltText('Page 1').evaluate((element: HTMLImageElement) => element.naturalWidth))
    .toBe(64)

  expect(assets.some(url => new URL(url).pathname.startsWith('/komga/js/'))).toBe(true)
  expect(assets.some(url => new URL(url).pathname.startsWith('/komga/css/'))).toBe(true)
  expect(requestPaths).toContain('/komga/sse/v1/events')
  expect(requestPaths).toContain(`/komga/api/v1/books/${books[0].id}/pages/1`)
  expect(escapedRequests).toEqual([])
})
