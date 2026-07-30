import fs from 'node:fs'
import path from 'node:path'
import zlib from 'node:zlib'

export interface FixturePaths {
  libraryRoot: string
  importRoot: string
}

export function createFixtures(root: string): FixturePaths {
  const libraryRoot = path.join(root, 'library')
  const importRoot = path.join(root, 'import-source')
  fs.mkdirSync(path.join(libraryRoot, 'Example Series'), {recursive: true})
  fs.mkdirSync(importRoot, {recursive: true})
  writeStoredZip(path.join(libraryRoot, 'Example Series', 'Volume 1.cbz'), [
    ['001.png', solidPng(64, 48, [220, 40, 40, 255])],
    ['002.png', solidPng(48, 64, [40, 80, 220, 255])],
    ['ComicInfo.xml', Buffer.from('<ComicInfo><Series>Example Series</Series><Title>Volume 1</Title><Number>1</Number></ComicInfo>')],
  ])
  writeStoredZip(path.join(importRoot, 'Import Candidate.cbz'), [
    ['001.png', solidPng(56, 56, [40, 180, 80, 255])],
    ['ComicInfo.xml', Buffer.from('<ComicInfo><Series>Example Series</Series><Title>Import Candidate</Title><Number>2</Number></ComicInfo>')],
  ])
  return {libraryRoot, importRoot}
}

function solidPng(width: number, height: number, color: [number, number, number, number]): Buffer {
  const row = Buffer.alloc(1 + width * 4)
  row[0] = 0
  for (let pixel = 0; pixel < width; pixel += 1) {
    row.set(color, 1 + pixel * 4)
  }
  const pixels = Buffer.concat(Array.from({length: height}, () => row))
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header[8] = 8
  header[9] = 6
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', header),
    pngChunk('IDAT', zlib.deflateSync(pixels)),
    pngChunk('IEND', Buffer.alloc(0)),
  ])
}

function pngChunk(type: string, contents: Buffer): Buffer {
  const typeBytes = Buffer.from(type)
  const chunk = Buffer.alloc(12 + contents.length)
  chunk.writeUInt32BE(contents.length, 0)
  typeBytes.copy(chunk, 4)
  contents.copy(chunk, 8)
  chunk.writeUInt32BE(crc32(Buffer.concat([typeBytes, contents])), 8 + contents.length)
  return chunk
}

function writeStoredZip(target: string, entries: Array<[string, Buffer]>): void {
  const localParts: Buffer[] = []
  const centralParts: Buffer[] = []
  let offset = 0
  for (const [name, contents] of entries) {
    const fileName = Buffer.from(name)
    const checksum = crc32(contents)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0, 6)
    local.writeUInt16LE(0, 8)
    local.writeUInt16LE(0, 10)
    local.writeUInt16LE(0x21, 12)
    local.writeUInt32LE(checksum, 14)
    local.writeUInt32LE(contents.length, 18)
    local.writeUInt32LE(contents.length, 22)
    local.writeUInt16LE(fileName.length, 26)
    local.writeUInt16LE(0, 28)
    localParts.push(local, fileName, contents)

    const central = Buffer.alloc(46)
    central.writeUInt32LE(0x02014b50, 0)
    central.writeUInt16LE(20, 4)
    central.writeUInt16LE(20, 6)
    central.writeUInt16LE(0, 8)
    central.writeUInt16LE(0, 10)
    central.writeUInt16LE(0, 12)
    central.writeUInt16LE(0x21, 14)
    central.writeUInt32LE(checksum, 16)
    central.writeUInt32LE(contents.length, 20)
    central.writeUInt32LE(contents.length, 24)
    central.writeUInt16LE(fileName.length, 28)
    central.writeUInt16LE(0, 30)
    central.writeUInt16LE(0, 32)
    central.writeUInt16LE(0, 34)
    central.writeUInt16LE(0, 36)
    central.writeUInt32LE(0, 38)
    central.writeUInt32LE(offset, 42)
    centralParts.push(central, fileName)
    offset += local.length + fileName.length + contents.length
  }

  const centralDirectory = Buffer.concat(centralParts)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(0, 4)
  end.writeUInt16LE(0, 6)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(centralDirectory.length, 12)
  end.writeUInt32LE(offset, 16)
  end.writeUInt16LE(0, 20)
  fs.writeFileSync(target, Buffer.concat([...localParts, centralDirectory, end]))
}

function crc32(bytes: Buffer): number {
  let crc = 0xffffffff
  for (const byte of bytes) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0)
    }
  }
  return (crc ^ 0xffffffff) >>> 0
}
