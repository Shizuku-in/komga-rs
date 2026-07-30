import fs from 'node:fs'
import path from 'node:path'

import {pollPageContent, signInAt} from './helpers'
import {expect, test} from './test'

test.use({seedLibrary: true})

test('visible MOVE import completes in the browser and moves the source file', async ({page, server}) => {
  test.setTimeout(120_000)
  await signInAt(page, server.url)
  const series = await pollPageContent(
    page,
    `${server.url}/api/v1/series?library_id=${server.libraryId}&unpaged=true`,
    1,
  )
  const seriesId = series[0].id as string
  await pollPageContent(
    page,
    `${server.url}/api/v1/books?series_id=${seriesId}&unpaged=true`,
    1,
    book => book.media.status === 'READY',
  )

  await page.goto(`${server.url}/import/books`)
  await page.getByLabel('Import from folder').fill(server.importRoot)
  await page.getByRole('button', {name: 'Scan', exact: true}).click()
  await expect(page.getByText('Import Candidate', {exact: true}).first()).toBeVisible()
  await expect(page.getByText('Example Series', {exact: true}).last()).toBeVisible({timeout: 20_000})

  await page.getByRole('button', {name: 'Hardlink/Copy Files', exact: true}).click()
  await page.getByText('Move Files', {exact: true}).last().click()
  await expect(page.getByRole('button', {name: 'Move Files', exact: true})).toBeVisible()

  const importButton = page.getByRole('main').getByRole('button', {name: 'Import', exact: true})
  await expect(importButton).toBeEnabled({timeout: 20_000})
  await importButton.click()
  await expect(page.getByText('Book imported successfully: Import Candidate', {exact: true})).toBeVisible({
    timeout: 40_000,
  })

  const books = await pollPageContent(page, `${server.url}/api/v1/books?series_id=${seriesId}&unpaged=true`, 2)
  expect(books.map(book => book.name).sort()).toEqual(['Import Candidate', 'Volume 1'])
  expect(fs.existsSync(path.join(server.importRoot, 'Import Candidate.cbz'))).toBe(false)
  expect(fs.existsSync(path.join(server.libraryRoot, 'Example Series', 'Import Candidate.cbz'))).toBe(true)
})
