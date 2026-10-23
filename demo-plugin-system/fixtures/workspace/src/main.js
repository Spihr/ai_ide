import { formatBytes } from './util/format.js'

export function summarize(files) {
  const total = files.reduce((sum, f) => sum + f.size, 0)
  return `${files.length} files, ${formatBytes(total)}`
}
