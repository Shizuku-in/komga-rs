import {getBookFormatFromMediaType} from '@/functions/book-format'

describe('Kindle format badges', () => {
  test.each([
    ['application/x-mobipocket-ebook', 'MOBI'],
    ['application/vnd.amazon.ebook', 'AZW'],
    ['application/vnd.amazon.mobi8-ebook', 'AZW3'],
  ])('maps %s to %s', (mediaType, expected) => {
    expect(getBookFormatFromMediaType(mediaType).type).toEqual(expected)
  })
})
