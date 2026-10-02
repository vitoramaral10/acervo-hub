import type { Movie } from '@/lib/api'

// Filtro, ordenação e escolhas lembradas da tela de Filmes: funções puras, sem React.

export type SortKey = 'titulo' | 'ano' | 'adicionado' | 'tamanho' | 'qualidade' | 'arquivo' | 'busca'
export type SortDir = 'asc' | 'desc'
export type StateFilter = 'todos' | 'com-arquivo' | 'sem-arquivo' | 'baixando' | 'na-fila' | 'falhou' | 'problemas'
export type QualityFilter = '2160p' | '1080p' | '720p'
export type AudioFilter = 'dual' | 'original'
export type MonitoredFilter = 'todos' | 'sim' | 'nao'
export type ViewMode = 'poster' | 'tabela'

export interface MovieListPrefs {
  sort: SortKey
  dir: SortDir
  state: StateFilter
  qualities: QualityFilter[]
  audio: AudioFilter[]
  monitored: MonitoredFilter
  view: ViewMode
}

export const DEFAULT_PREFS: MovieListPrefs = {
  sort: 'titulo',
  dir: 'asc',
  state: 'todos',
  qualities: [],
  audio: [],
  monitored: 'todos',
  view: 'poster',
}

export const SORTS: { value: SortKey; label: string }[] = [
  { value: 'titulo', label: 'Título' },
  { value: 'ano', label: 'Ano' },
  { value: 'adicionado', label: 'Adicionado ao acervo' },
  { value: 'tamanho', label: 'Tamanho do arquivo' },
  { value: 'qualidade', label: 'Qualidade' },
  { value: 'arquivo', label: 'Data do arquivo' },
  { value: 'busca', label: 'Última busca' },
]

export const STATES: { value: StateFilter; label: string }[] = [
  { value: 'todos', label: 'Todos' },
  { value: 'com-arquivo', label: 'No disco' },
  { value: 'sem-arquivo', label: 'Faltando' },
  { value: 'baixando', label: 'Baixando' },
  { value: 'na-fila', label: 'Na fila' },
  { value: 'falhou', label: 'Falhou' },
  { value: 'problemas', label: 'Com problema' },
]

export const QUALITIES: { value: QualityFilter; label: string }[] = [
  { value: '2160p', label: '2160p' },
  { value: '1080p', label: '1080p' },
  { value: '720p', label: '720p ou menos' },
]

export const AUDIOS: { value: AudioFilter; label: string }[] = [
  { value: 'dual', label: 'Dual áudio' },
  { value: 'original', label: 'Só original' },
]

export const MONITORED: { value: MonitoredFilter; label: string }[] = [
  { value: 'todos', label: 'Todos' },
  { value: 'sim', label: 'Monitorados' },
  { value: 'nao', label: 'Não monitorados' },
]

/** Direção que costuma interessar ao trocar de campo: título de A a Z, o resto do maior/mais novo. */
export const defaultDir = (sort: SortKey): SortDir => (sort === 'titulo' ? 'asc' : 'desc')

export function normalize(text: string) {
  return text
    .normalize('NFD')
    .replace(/\p{Diacritic}/gu, '')
    .toLowerCase()
}

/** Ordem de estante: sem artigo inicial. */
export function titleKey(movie: Movie) {
  return normalize(movie.titulo).replace(/^(the|a|an|o|os|as|um|uma)\s+/, '')
}

export const hasProblem = (movie: Movie) => movie.arquivo !== null && movie.arquivo.disco !== 'ok'

/** Mesma frase de `QUEUED` em crates/acervo-hub/src/grab.rs. */
const QUEUED_MESSAGE = 'na fila: aguardando espaço'

export const isQueued = (movie: Movie) =>
  movie.download?.estado === 'downloading' && normalize(movie.download.mensagem ?? '').startsWith(normalize(QUEUED_MESSAGE))

export const isDownloading = (movie: Movie) => movie.download?.estado === 'downloading' && !isQueued(movie)

export const hasFailed = (movie: Movie) => movie.download?.estado === 'failed' && !movie.arquivo

export function matchesState(movie: Movie, state: StateFilter): boolean {
  switch (state) {
    case 'todos':
      return true
    case 'com-arquivo':
      return movie.arquivo !== null
    case 'sem-arquivo':
      return movie.arquivo === null
    case 'baixando':
      return isDownloading(movie)
    case 'na-fila':
      return isQueued(movie)
    case 'falhou':
      return hasFailed(movie)
    case 'problemas':
      return hasProblem(movie)
  }
}

/** Resolução vertical da qualidade ("WEBDL-1080p" → 1080); BR-DISK não traz número e é 1080p. */
export function resolutionOf(quality: string | null | undefined): number | null {
  if (!quality) return null
  const match = /(\d{3,4})p/i.exec(quality)
  if (match) return Number(match[1])
  return /br-?disk/i.test(quality) ? 1080 : null
}

const SOURCE_RANK: [RegExp, number][] = [
  [/remux|br-?disk/i, 6],
  [/bluray/i, 5],
  [/web-?dl/i, 4],
  [/web-?rip/i, 3],
  [/hdtv/i, 2],
  [/dvd/i, 1],
]

/** Resolução primeiro, depois a origem (DVD < HDTV < WEBRip < WEBDL < Bluray < Remux). */
export function qualityRank(quality: string | null | undefined): number | null {
  const resolution = resolutionOf(quality)
  if (resolution === null || !quality) return null
  const source = SOURCE_RANK.find(([pattern]) => pattern.test(quality))?.[1] ?? 0
  return resolution * 10 + source
}

function matchesQuality(movie: Movie, qualities: QualityFilter[]): boolean {
  if (qualities.length === 0) return true
  const resolution = resolutionOf(movie.arquivo?.qualidade)
  if (resolution === null) return false
  const bucket: QualityFilter = resolution >= 2160 ? '2160p' : resolution >= 1080 ? '1080p' : '720p'
  return qualities.includes(bucket)
}

/** Nome ("Portuguese", "Portuguese (Brazil)") ou código ("pt", "pt-BR", "por"). */
export const isPortuguese = (language: string) => /^(portuguese|portugues|pt\b|por\b)/i.test(normalize(language).trim())

/** Sem idiomas conhecidos (ou só português) não cai em nenhum dos dois grupos. */
function audioGroups(movie: Movie): AudioFilter[] {
  const languages = movie.arquivo?.idiomas ?? []
  if (languages.length === 0) return []
  const portuguese = languages.some(isPortuguese)
  if (!portuguese) return ['original']
  return languages.some((language) => !isPortuguese(language)) ? ['dual'] : []
}

function matchesAudio(movie: Movie, audio: AudioFilter[]): boolean {
  if (audio.length === 0) return true
  return audioGroups(movie).some((group) => audio.includes(group))
}

function matchesMonitored(movie: Movie, monitored: MonitoredFilter): boolean {
  return monitored === 'todos' || movie.monitorado === (monitored === 'sim')
}

function matchesQuery(movie: Movie, needle: string): boolean {
  if (!needle) return true
  const haystack = normalize(
    [movie.titulo, movie.titulo_original ?? '', String(movie.ano ?? ''), movie.imdb ?? ''].join(' '),
  )
  return haystack.includes(needle)
}

/** Tudo menos o estado: base dos contadores das opções de estado. */
export function filterExceptState(movies: Movie[], query: string, prefs: MovieListPrefs): Movie[] {
  const needle = normalize(query.trim())
  return movies.filter(
    (movie) =>
      matchesQuery(movie, needle) &&
      matchesQuality(movie, prefs.qualities) &&
      matchesAudio(movie, prefs.audio) &&
      matchesMonitored(movie, prefs.monitored),
  )
}

export function stateCounts(movies: Movie[]): Record<StateFilter, number> {
  const counts = {} as Record<StateFilter, number>
  for (const { value } of STATES) counts[value] = movies.filter((movie) => matchesState(movie, value)).length
  return counts
}

const time = (iso: string | null | undefined): number | null => {
  if (!iso) return null
  const value = Date.parse(iso)
  return Number.isNaN(value) ? null : value
}

/** Valor numérico do campo de ordenação; `null` = não tem, vai para o fim. */
export function sortValue(movie: Movie, sort: SortKey): number | null {
  switch (sort) {
    case 'titulo':
      return 0
    case 'ano':
      return movie.ano
    case 'adicionado':
      return time(movie.adicionado)
    case 'tamanho':
      return movie.arquivo ? movie.arquivo.tamanho : null
    case 'qualidade':
      return qualityRank(movie.arquivo?.qualidade)
    case 'arquivo':
      return time(movie.arquivo?.adicionado)
    case 'busca':
      return time(movie.ultima_busca?.quando)
  }
}

export function sortMovies(movies: Movie[], sort: SortKey, dir: SortDir): Movie[] {
  const sign = dir === 'asc' ? 1 : -1
  const keys = new Map(movies.map((movie) => [movie.id, titleKey(movie)]))
  const byTitle = (a: Movie, b: Movie) => (keys.get(a.id) ?? '').localeCompare(keys.get(b.id) ?? '', 'pt-BR')
  return [...movies].sort((a, b) => {
    if (sort === 'titulo') return sign * byTitle(a, b)
    const x = sortValue(a, sort)
    const y = sortValue(b, sort)
    // Sem valor sempre no fim, nas duas direções.
    if (x === null && y === null) return byTitle(a, b)
    if (x === null) return 1
    if (y === null) return -1
    return x === y ? byTitle(a, b) : sign * (x - y)
  })
}

export function applyList(movies: Movie[], query: string, prefs: MovieListPrefs): Movie[] {
  const filtered = filterExceptState(movies, query, prefs).filter((movie) => matchesState(movie, prefs.state))
  return sortMovies(filtered, prefs.sort, prefs.dir)
}

/** Algum filtro (não ordem nem visualização) fora do padrão. */
export const hasActiveFilters = (prefs: MovieListPrefs) =>
  prefs.state !== 'todos' || prefs.qualities.length > 0 || prefs.audio.length > 0 || prefs.monitored !== 'todos'

// ---------------------------------------------------------------- escolhas lembradas

const STORAGE_KEY = 'acervo.filmes.lista'
const KEYS = ['ordem', 'dir', 'estado', 'qualidade', 'audio', 'monitorado', 'vista'] as const

const pick = <T extends string>(value: string | null | undefined, options: readonly { value: T }[], fallback: T): T =>
  options.find((option) => option.value === value)?.value ?? fallback

const pickMany = <T extends string>(value: string | null | undefined, options: readonly { value: T }[]): T[] => {
  const wanted = (value ?? '').split(',')
  return options.map((option) => option.value).filter((option) => wanted.includes(option))
}

const VIEWS: { value: ViewMode }[] = [{ value: 'poster' }, { value: 'tabela' }]
const DIRS: { value: SortDir }[] = [{ value: 'asc' }, { value: 'desc' }]

export function prefsFromParams(params: URLSearchParams): MovieListPrefs {
  const sort = pick(params.get('ordem'), SORTS, DEFAULT_PREFS.sort)
  return {
    sort,
    dir: pick(params.get('dir'), DIRS, defaultDir(sort)),
    state: pick(params.get('estado'), STATES, DEFAULT_PREFS.state),
    qualities: pickMany(params.get('qualidade'), QUALITIES),
    audio: pickMany(params.get('audio'), AUDIOS),
    monitored: pick(params.get('monitorado'), MONITORED, DEFAULT_PREFS.monitored),
    view: pick(params.get('vista'), VIEWS, DEFAULT_PREFS.view),
  }
}

/** Só o que difere do padrão (o padrão da direção depende do campo, ver `defaultDir`). */
export function prefsToParams(prefs: MovieListPrefs): URLSearchParams {
  const params = new URLSearchParams()
  if (prefs.sort !== DEFAULT_PREFS.sort) params.set('ordem', prefs.sort)
  if (prefs.dir !== defaultDir(prefs.sort)) params.set('dir', prefs.dir)
  if (prefs.state !== 'todos') params.set('estado', prefs.state)
  if (prefs.qualities.length > 0) params.set('qualidade', prefs.qualities.join(','))
  if (prefs.audio.length > 0) params.set('audio', prefs.audio.join(','))
  if (prefs.monitored !== 'todos') params.set('monitorado', prefs.monitored)
  if (prefs.view !== 'poster') params.set('vista', prefs.view)
  return params
}

/** Query do hash atual (`#filmes?ordem=ano`). */
export function hashParams(hash = window.location.hash): URLSearchParams {
  const at = hash.indexOf('?')
  return new URLSearchParams(at === -1 ? '' : hash.slice(at + 1))
}

/** URL manda; sem nenhum parâmetro nosso nela, vale o último estado guardado. */
export function loadPrefs(): MovieListPrefs {
  const fromUrl = hashParams()
  if (KEYS.some((key) => fromUrl.has(key))) return prefsFromParams(fromUrl)
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored !== null) return prefsFromParams(new URLSearchParams(stored))
  } catch {
    // Armazenamento bloqueado: segue com o padrão.
  }
  return DEFAULT_PREFS
}

export function savePrefs(prefs: MovieListPrefs) {
  const query = prefsToParams(prefs).toString()
  try {
    localStorage.setItem(STORAGE_KEY, query)
  } catch {
    // Sem armazenamento, só a URL lembra.
  }
  // replaceState: não enche o histórico nem dispara `hashchange`.
  const url = `${window.location.pathname}${window.location.search}#filmes${query ? `?${query}` : ''}`
  if (window.location.hash !== url.slice(url.indexOf('#'))) history.replaceState(null, '', url)
}
