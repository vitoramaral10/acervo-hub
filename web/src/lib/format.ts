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

/** "14:35", no fuso do navegador; "—" se a data não existe. */
export function formatClock(iso: string | null | undefined): string {
  if (!iso) return '—'
  return new Date(iso).toLocaleTimeString('pt-BR', { hour: '2-digit', minute: '2-digit' })
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

/** "850 ms", "12 s", "3 min 20 s", "1 h 5 min". */
export function formatDuration(ms: number | null | undefined): string {
  if (ms == null) return '—'
  if (ms < 1000) return `${integer.format(Math.round(ms))} ms`
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds} s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return seconds % 60 ? `${minutes} min ${seconds % 60} s` : `${minutes} min`
  const hours = Math.floor(minutes / 60)
  return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`
}

/** Intervalo de agendamento: "a cada 5 min", "a cada 6 h"; zero é "só manual". */
export function formatInterval(minutes: number): string {
  if (minutes <= 0) return 'só manual'
  if (minutes % 1440 === 0) return minutes === 1440 ? 'diária' : `a cada ${minutes / 1440} dias`
  if (minutes % 60 === 0) return `a cada ${minutes / 60} h`
  return `a cada ${minutes} min`
}
