const UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB']

const decimal = new Intl.NumberFormat('pt-BR', { maximumFractionDigits: 1, minimumFractionDigits: 1 })
const integer = new Intl.NumberFormat('pt-BR')
const relative = new Intl.RelativeTimeFormat('pt-BR', { numeric: 'auto' })

export function formatSize(bytes: number): string {
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return unit === 0 ? `${integer.format(value)} B` : `${decimal.format(value)} ${UNITS[unit]}`
}

export function formatCount(value: number | null | undefined): string {
  return value == null ? '—' : integer.format(value)
}

const STEPS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 24 * 3600],
  ['month', 30 * 24 * 3600],
  ['week', 7 * 24 * 3600],
  ['day', 24 * 3600],
  ['hour', 3600],
  ['minute', 60],
]

/** "há 3 horas", "ontem". Abaixo de um minuto, "agora". */
export function formatAgo(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return '—'
  const seconds = (new Date(iso).getTime() - now) / 1000
  for (const [unit, size] of STEPS) {
    if (Math.abs(seconds) >= size) return relative.format(Math.round(seconds / size), unit)
  }
  return 'agora'
}

export function ageInSeconds(iso: string | null | undefined, now = Date.now()): number {
  return iso ? (now - new Date(iso).getTime()) / 1000 : Number.POSITIVE_INFINITY
}

/** Link externo só se for http(s): título e link vêm do tracker. */
export function safeHref(value: string | null | undefined): string | undefined {
  if (!value) return undefined
  if (value.startsWith('/ui/')) return value
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:' || url.protocol === 'magnet:'
      ? url.toString()
      : undefined
  } catch {
    return undefined
  }
}
