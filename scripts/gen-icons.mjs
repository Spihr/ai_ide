/**
 * 生成占位图标（一次性脚本，产物写入 apps/desktop/build/）。
 *
 * 输出：
 *   build/icon.ico          多尺寸 ICO（16/24/32/48/64/128/256，均以 PNG 压缩条目内嵌）
 *   build/icon.png          512×512 PNG（electron-builder 通用图标源）
 *   build/icons/<n>x<n>.png 16/32/48/64/128/256/512 PNG 集（Linux 打包用）
 *
 * 无第三方依赖：PNG 由 zlib 手写编码，ICO 为 PNG 条目的容器。
 */

import { deflateSync } from 'node:zlib'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

// ---------------------------------------------------------------------------
// 绘制：深色圆角方底 + 蓝色渐变主色 + 白色 "A" 字几何
// ---------------------------------------------------------------------------

const BG = [20, 22, 26]
const PRIMARY = [31, 111, 235]
const ACCENT = [88, 166, 255]
const FG = [246, 248, 250]

function insideRoundedRect(x, y, size, radius) {
  const r = radius
  const cx = Math.min(Math.max(x, r), size - r)
  const cy = Math.min(Math.max(y, r), size - r)
  const dx = x - cx
  const dy = y - cy
  return dx * dx + dy * dy <= r * r
}

function distanceToSegment(px, py, ax, ay, bx, by) {
  const vx = bx - ax
  const vy = by - ay
  const wx = px - ax
  const wy = py - ay
  const len2 = vx * vx + vy * vy
  const t = len2 === 0 ? 0 : Math.min(1, Math.max(0, (wx * vx + wy * vy) / len2))
  const dx = px - (ax + t * vx)
  const dy = py - (ay + t * vy)
  return Math.sqrt(dx * dx + dy * dy)
}

/** 返回该像素的 [r,g,b,a]；a 为 0..255 的覆盖率（超采样抗锯齿）。 */
function renderPixel(x, y, size) {
  const s = size
  const radius = s * 0.22
  if (!insideRoundedRect(x, y, s, radius)) return [0, 0, 0, 0]

  // 背景：对角渐变 BG → PRIMARY
  const t = (x + y) / (2 * s)
  let r = BG[0] + (PRIMARY[0] - BG[0]) * t
  let g = BG[1] + (PRIMARY[1] - BG[1]) * t
  let b = BG[2] + (PRIMARY[2] - BG[2]) * t

  // "N" 字标记（仅字母几何本身，不承载任何产品名文字/品牌文案）
  const stroke = Math.max(1, s * 0.085)
  const leftX = s * 0.3
  const rightX = s * 0.7
  const topY = s * 0.24
  const bottomY = s * 0.76

  const d = Math.min(
    distanceToSegment(x, y, leftX, topY, leftX, bottomY),
    distanceToSegment(x, y, rightX, topY, rightX, bottomY),
    distanceToSegment(x, y, leftX, topY, rightX, bottomY)
  )
  const ink = Math.min(1, Math.max(0, (stroke / 2 - d + 0.5) / 1))
  if (ink > 0) {
    const mix = ink
    r = r * (1 - mix) + FG[0] * mix
    g = g * (1 - mix) + FG[1] * mix
    b = b * (1 - mix) + FG[2] * mix
  }

  // 左上高光，让底色不那么平
  const hl = Math.max(0, 1 - Math.hypot(x - s * 0.2, y - s * 0.2) / (s * 0.55))
  r = r * (1 - hl * 0.12) + ACCENT[0] * hl * 0.12
  g = g * (1 - hl * 0.12) + ACCENT[1] * hl * 0.12
  b = b * (1 - hl * 0.12) + ACCENT[2] * hl * 0.12

  return [Math.round(r), Math.round(g), Math.round(b), 255]
}

/** 以 3×3 超采样渲染一张 RGBA 图（size×size）。 */
function renderRgba(size) {
  const ss = 3
  const out = Buffer.alloc(size * size * 4)
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let r = 0
      let g = 0
      let b = 0
      let a = 0
      for (let sy = 0; sy < ss; sy += 1) {
        for (let sx = 0; sx < ss; sx += 1) {
          const px = renderPixel(x + (sx + 0.5) / ss, y + (sy + 0.5) / ss, size)
          r += px[0] * px[3]
          g += px[1] * px[3]
          b += px[2] * px[3]
          a += px[3]
        }
      }
      const n = ss * ss
      const idx = (y * size + x) * 4
      if (a === 0) {
        out[idx] = 0
        out[idx + 1] = 0
        out[idx + 2] = 0
        out[idx + 3] = 0
      } else {
        out[idx] = Math.round(r / a)
        out[idx + 1] = Math.round(g / a)
        out[idx + 2] = Math.round(b / a)
        out[idx + 3] = Math.round(a / n)
      }
    }
  }
  return out
}

// ---------------------------------------------------------------------------
// PNG 编码（真彩 + alpha，无滤波）
// ---------------------------------------------------------------------------

const CRC_TABLE = (() => {
  const table = new Int32Array(256)
  for (let n = 0; n < 256; n += 1) {
    let c = n
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    table[n] = c
  }
  return table
})()

function crc32(buf) {
  let c = 0xffffffff
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length, 0)
  const typeBuf = Buffer.from(type, 'ascii')
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0)
  return Buffer.concat([len, typeBuf, data, crc])
}

function encodePng(rgba, size) {
  const raw = Buffer.alloc((size * 4 + 1) * size)
  for (let y = 0; y < size; y += 1) {
    raw[y * (size * 4 + 1)] = 0
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4)
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(size, 0)
  ihdr.writeUInt32BE(size, 4)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // RGBA
  ihdr[10] = 0
  ihdr[11] = 0
  ihdr[12] = 0
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0))
  ])
}

// ---------------------------------------------------------------------------
// ICO 容器（各尺寸以 PNG 条目内嵌）
// ---------------------------------------------------------------------------

function encodeIco(entries) {
  const header = Buffer.alloc(6)
  header.writeUInt16LE(0, 0)
  header.writeUInt16LE(1, 2)
  header.writeUInt16LE(entries.length, 4)

  const directory = Buffer.alloc(16 * entries.length)
  let offset = header.length + directory.length
  entries.forEach((entry, i) => {
    const base = i * 16
    directory[base] = entry.size >= 256 ? 0 : entry.size
    directory[base + 1] = entry.size >= 256 ? 0 : entry.size
    directory[base + 2] = 0
    directory[base + 3] = 0
    directory.writeUInt16LE(1, base + 4)
    directory.writeUInt16LE(32, base + 6)
    directory.writeUInt32BE(0, base + 8)
    directory.writeUInt32LE(entry.png.length, base + 8)
    directory.writeUInt32LE(offset, base + 12)
    offset += entry.png.length
  })

  return Buffer.concat([header, directory, ...entries.map((e) => e.png)])
}

// ---------------------------------------------------------------------------

const buildDir = process.argv[2]
if (!buildDir) {
  console.error('用法：node gen-icons.mjs <build 目录绝对路径>')
  process.exit(1)
}

const icoSizes = [16, 24, 32, 48, 64, 128, 256]
const pngSizes = [16, 32, 48, 64, 128, 256, 512]

const rendered = new Map()
for (const size of new Set([...icoSizes, ...pngSizes])) {
  rendered.set(size, encodePng(renderRgba(size), size))
}

mkdirSync(join(buildDir, 'icons'), { recursive: true })
for (const size of pngSizes) {
  writeFileSync(join(buildDir, 'icons', `${size}x${size}.png`), rendered.get(size))
}
writeFileSync(join(buildDir, 'icon.png'), rendered.get(512))
writeFileSync(
  join(buildDir, 'icon.ico'),
  encodeIco(icoSizes.map((size) => ({ size, png: rendered.get(size) })))
)

console.log(JSON.stringify({
  buildDir,
  ico: join(buildDir, 'icon.ico'),
  icoSizes,
  iconPng: join(buildDir, 'icon.png'),
  pngDir: join(buildDir, 'icons'),
  pngSizes
}, null, 2))

// 目录参数仅用于定位输出位置
void dirname
