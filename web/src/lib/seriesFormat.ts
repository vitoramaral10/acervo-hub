import type { Episode, SeriesSummary } from '@/lib/api'
import { normalize } from '@/lib/movieList'

// Texto e contas da tela de Séries: funções puras, sem React.

const shortDate = new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit' })
const fullDate = new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit', year: 'numeric' })

/** Dia de exibição (`AAAA-MM-DD`) como "02/10/2026"; sem data, "sem data". */
export function formatAirDate(day: string | null | undefined): string {
  const match = day ? /^(\d{4})-(\d{2})-(\d{2})/.exec(day) : null
  return match ? `${match[3]}/${match[2]}/${match[1]}` : 'sem data'
}

/** "02/10" no ano corrente, "02/10/2025" fora dele. */
function formatEventDay(iso: string | null | undefined, now = new Date()): string | null {
  if (!iso) return null
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return null
  return (date.getFullYear() === now.getFullYear() ? shortDate : fullDate).format(date)
}

/** O chip de um episódio dispensado: "apagado 02/10", "não quero". */
export function skipLabel(episode: Episode): string {
  const day = formatEventDay(episode.motivo_em)
  const text = episode.motivo === 'deleted' ? 'apagado' : 'não quero'
  return day ? `${text} ${day}` : text
}

export const seasonName = (number: number) => (number === 0 ? 'Especiais' : `Temporada ${number}`)

/** "S02E03". */
export const episodeCode = (season: number, episode: number) =>
  `S${String(season).padStart(2, '0')}E${String(episode).padStart(2, '0')}`

const STATUS: Record<string, string> = {
  'Returning Series': 'Em exibição',
  'In Production': 'Em produção',
  Planned: 'Planejada',
  Pilot: 'Piloto',
  Ended: 'Encerrada',
  Canceled: 'Cancelada',
}

export const seriesStatus = (status: string) => STATUS[status] ?? status

/** Motivos de rejeição de série, na voz da tela. */
export const SERIES_REASONS: Record<string, string> = {
  UnknownSeries: 'de outra série',
  WrongSeries: 'de outra série',
  UnparsableEpisode: 'sem temporada nem episódio no nome',
  NothingWanted: 'nada que falte',
  NotAired: 'ainda não foi ao ar',
  AlreadyQueued: 'já baixando',
  UnableToParse: 'nome ilegível',
  QualityNotWanted: 'qualidade fora do perfil',
  BelowMinimumSize: 'pequeno demais',
  AboveMaximumSize: 'grande demais',
  MaximumSizeExceeded: 'acima do teto',
  MinimumSeeders: 'poucos seeders',
  Raw: 'disco bruto',
  HardcodeSubtitles: 'legenda embutida',
  Sample: 'amostra',
}

export type SeriesFilter = 'todas' | 'faltando' | 'baixando'

export const SERIES_FILTERS: { value: SeriesFilter; label: string }[] = [
  { value: 'todas', label: 'Todas' },
  { value: 'faltando', label: 'Faltando' },
  { value: 'baixando', label: 'Baixando' },
]

export function matchesFilter(series: SeriesSummary, filter: SeriesFilter): boolean {
  if (filter === 'faltando') return series.episodios.quero_exibidos > 0
  if (filter === 'baixando') return series.episodios.baixando > 0
  return true
}

export function matchesQuery(series: SeriesSummary, query: string): boolean {
  const needle = normalize(query.trim())
  if (!needle) return true
  return [series.titulo, series.titulo_original, series.titulo_ingles, series.rede, series.ano, series.imdb]
    .filter((field) => field != null)
    .some((field) => normalize(String(field)).includes(needle))
}

/** Ordem de estante: sem artigo inicial. */
export const seriesTitleKey = (series: SeriesSummary) =>
  normalize(series.titulo).replace(/^(the|a|an|o|os|as|um|uma)\s+/, '')

/** Arquivos distintos dos episódios (um arquivo de multi-episódio conta uma vez). */
export function distinctFiles(episodes: Episode[]): number {
  return new Set(episodes.filter((e) => e.arquivo).map((e) => e.arquivo!.id)).size
}
