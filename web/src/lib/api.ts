// Cliente da API da interface. Toda ação que muda estado manda `X-Acervo`,
// que o servidor exige — um formulário de outra origem não consegue mandá-lo.

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message)
  }
}

export const isUnauthorized = (error: unknown) => error instanceof ApiError && error.status === 401

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (method !== 'GET') headers['X-Acervo'] = '1'
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  let response: Response
  try {
    response = await fetch(path, {
      method,
      headers,
      credentials: 'same-origin',
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  } catch {
    throw new ApiError(0, 'Sem conexão com o acervo-hub.')
  }
  const data = await response.json().catch(() => ({}))
  if (!response.ok) {
    const message = typeof data?.erro === 'string' ? data.erro : `Falha inesperada (HTTP ${response.status}).`
    throw new ApiError(response.status, message)
  }
  return data as T
}

interface Health {
  ultimo_sucesso: string | null
  ultima_falha: string | null
  ultimo_erro: string | null
  falhas_seguidas: number
  resultados: number | null
  /** RFC 3339; só vem preenchido enquanto o tracker pediu para recuar (HTTP 429). */
  em_espera_ate: string | null
}

export interface Indexer {
  nome: string
  ativo: boolean
  /** Todo indexador é cadastro do banco; o rótulo fica só por compatibilidade. */
  origem: string
  privado: boolean
  editavel: boolean
  modos: { busca: boolean; series: boolean; filmes: boolean }
  categorias: { id: number; nome: string }[]
  saude: Health
}

export interface TestResult {
  ok: boolean
  resultados?: number
  erro?: string
}

export interface Setting {
  name: string
  label: string
  kind: 'text' | 'password' | 'checkbox' | 'select'
  options: string[]
  secret: boolean
  is_set: boolean
  value: string | null
}

export interface Release {
  titulo: string
  indexador: string
  tamanho: number
  seeders: number | null
  leechers: number | null
  downloads: number | null
  categorias: number[]
  publicado: string | null
  detalhes: string | null
  download: string
  /** O link do indexador, para mandar ao cliente pela busca. */
  link: string
}

/** Um dia da série de um indexador. */
export interface IndexerStatsDay {
  dia: string
  consultas: number
  falhas: number
  grabs: number
}

/** Os números de um indexador na janela pedida. */
export interface IndexerStats {
  nome: string
  consultas: number
  falhas: number
  /** Respostas 429. */
  limitadas: number
  grabs: number
  /** De 0 a 1. */
  taxa_falha: number
  tempo_medio_ms: number | null
  serie: IndexerStatsDay[]
}

export interface SentToClient {
  ok: boolean
  hash: string
  categoria: string
  magnet: boolean
}

export interface SearchResponse {
  resultados: Release[]
  falhas: { indexador: string; erro: string }[]
}

export interface Definition {
  id: string
  name: string
  description: string
  language: string
  private: boolean
  supported: boolean
  reason: string | null
  added: boolean
}

export interface CycleReport {
  quando: string
  torrents: number
  ilegiveis: { nome: string; motivo: string }[]
  biblioteca: string
  abortado: string | null
  acoes: { tipo: string; titulo: string; detalhe: string }[]
  espaco: string
  pulados: { motivo: string; quantos: number }[]
  executadas: number | null
  falharam: number | null
}

/** Como uma execução de tarefa terminou. */
interface TaskLastRun {
  inicio: string
  fim: string
  duracao_ms: number
  ok: boolean
  resumo: string
}

/** Uma tarefa de fundo do serviço; `intervalo_minutos` 0 é desligada no agendamento. */
export interface Task {
  id: string
  nome: string
  intervalo_minutos: number
  /** Por que a tarefa está fora do agendamento (falta configuração), ou `null`. */
  indisponivel: string | null
  rodando: boolean
  iniciada_em: string | null
  /** Quanto falta, como "3 de 10", quando a tarefa sabe dizer. */
  andamento: string | null
  proxima: string | null
  ultima: TaskLastRun | null
}

/** O detalhe de "Atualização de metadados": o que mudou e o que falhou, filme a filme. */
export interface MetadataReport {
  atualizados: string[]
  falhas: { filme: string; erro: string }[]
}

/** Uma execução no histórico; `detalhe` é o relatório dela (limpeza: `CycleReport`; metadados: `MetadataReport`). */
export interface TaskRun extends TaskLastRun {
  id: number
  tarefa: string
  nome: string
  detalhe: unknown
}

interface MovieFile {
  nome: string
  tamanho: number
  qualidade: string
  idiomas: string[]
  grupo: string | null
  edicao: string | null
  release: string | null
  adicionado: string | null
  disco: 'ok' | 'missing' | 'size_differs' | 'unreadable'
  disco_detalhe: string | null
}

export interface Movie {
  id: number
  tmdb: number
  imdb: string | null
  titulo: string
  titulo_original: string | null
  ano: number | null
  poster: string | null
  sinopse: string | null
  status: string | null
  monitorado: boolean
  prioritario: boolean
  pasta: string
  adicionado: string | null
  arquivo: MovieFile | null
  ultima_busca: LastSearch | null
  download: MovieDownload | null
}

export interface MovieDownload {
  estado: 'downloading' | 'imported' | 'failed'
  release: string
  mensagem: string | null
  pego_em: string
}

export interface Configuration {
  tmdb: { definida: boolean }
}

/** Andamento da busca dos filmes que faltam; `iniciada` só vale na resposta do POST. */
export interface MissingSearch {
  iniciada: boolean
  rodando: boolean
  buscados: number
  total: number
}

export interface GrabReport {
  filme: string
  releases: number
  escolhido: { titulo: string; indexador: string; qualidade: string; tamanho: number } | null
  motivos: [string, number][]
  aplicado: boolean
}

export interface LastSearch {
  quando: string
  releases: number
  escolhido: string | null
  qualidade: string | null
  motivos: [string, number][]
  erro: string | null
}

// ---------------------------------------------------------------- configuração

/** Segredo como a API o devolve: nunca o valor, só se está definido. */
interface Secret {
  definida: boolean
}

export interface ServerSection {
  api_key: Secret
  catalogos: string[]
  /** O `.tar.gz` do repositório de definições. */
  definicoes_url: string
  http_timeout_seconds: number
  /** Base do XEM, de onde vem a numeração de cena. */
  xem_url: string
  flaresolverr_url: string | null
  flaresolverr_timeout_s: number
  proxy_url: string | null
  proxy_username: string
  proxy_password: Secret
}

export interface DownloadClientSection {
  url: string
  username: string
  password: Secret
}

export interface JellyfinSection {
  url: string
  api_key: Secret
  carencia_sugestao_minutos: number
}

export interface LibrarySection {
  roots: string[]
  root_folders: string[]
  category: string
  /** Categoria do que a busca manual manda ao cliente; fora da limpeza. */
  categoria_manual: string
  paths: Record<string, string>
}

export interface CleanupSection {
  orphan_strikes: number
  delete_private_orphans: boolean
  private_seed_grace_hours: number
  seed_ratio_alvo: number
  seed_ocioso_horas: number
  recent_change_grace_hours: number
  max_batch_gib: number
  max_batch_fraction: number
  managed_categories: string[]
}

export interface TasksSection {
  intervalos: Record<string, number>
  search_limit: number
}

/** As seções e o formato de cada uma, como o GET devolve. */
export interface Sections {
  servidor: ServerSection
  qbittorrent: DownloadClientSection
  jellyfin: JellyfinSection
  biblioteca: LibrarySection
  limpeza: CleanupSection
  tarefas: TasksSection
}

export type SectionName = keyof Sections

/** No PUT, segredo vai como texto; vazio ou ausente mantém o guardado. */
type Input<T> = T extends Secret ? string : T extends (infer U)[] ? Input<U>[] : T extends object ? { [K in keyof T]?: Input<T[K]> } : T

export type SectionInput<N extends SectionName> = Input<Sections[N]>

export const api = {
  session: () => request<{ ok: boolean; usuario: string | null }>('GET', '/ui/api/sessao'),
  login: (credentials: { usuario: string; senha: string }) =>
    request<{ ok: boolean; usuario: string }>('POST', '/ui/api/entrar', credentials),
  logout: () => request<{ ok: boolean }>('POST', '/ui/api/sair'),
  indexers: () => request<{ indexadores: Indexer[] }>('GET', '/ui/api/indexadores'),
  test: (name: string) =>
    request<TestResult>('POST', `/ui/api/indexadores/${encodeURIComponent(name)}/testar`),
  settings: (name: string) =>
    request<{ settings: Setting[] }>('GET', `/ui/api/indexadores/${encodeURIComponent(name)}/settings`),
  saveSettings: (name: string, values: Record<string, string>) =>
    request<{ ok: boolean; teste: TestResult }>(
      'PUT',
      `/ui/api/indexadores/${encodeURIComponent(name)}/settings`,
      values,
    ),
  catalog: () => request<{ definicoes: Definition[] }>('GET', '/ui/api/catalogo'),
  definitionSettings: (id: string) =>
    request<{ settings: Setting[] }>('GET', `/ui/api/catalogo/${encodeURIComponent(id)}/settings`),
  addIndexer: (definicao: string, settings: Record<string, string>) =>
    request<{ ok: boolean; nome: string; teste: TestResult }>('POST', '/ui/api/indexadores', {
      definicao,
      settings,
    }),
  removeIndexer: (name: string) =>
    request<{ ok: boolean }>('DELETE', `/ui/api/indexadores/${encodeURIComponent(name)}`),
  setEnabled: (name: string, ativo: boolean) =>
    request<{ ok: boolean }>('PUT', `/ui/api/indexadores/${encodeURIComponent(name)}/ativo`, { ativo }),
  tasks: () => request<{ tarefas: Task[] }>('GET', '/ui/api/tarefas'),
  taskHistory: () => request<{ historico: TaskRun[] }>('GET', '/ui/api/tarefas/historico'),
  runTask: (id: string) =>
    request<{ iniciada: boolean; tarefa: Task | null }>('POST', `/ui/api/tarefas/${encodeURIComponent(id)}/rodar`),
  movies: () => request<{ filmes: Movie[] }>('GET', '/ui/api/filmes'),
  searchMissing: () => request<MissingSearch>('POST', '/ui/api/filmes/buscar'),
  missingSearch: () => request<MissingSearch>('GET', '/ui/api/filmes/buscar'),
  grab: (id: number, aplicar: boolean) =>
    request<GrabReport>('POST', `/ui/api/filmes/${id}/pegar`, { aplicar }),
  configuration: () => request<Configuration>('GET', '/ui/api/configuracoes'),
  saveConfiguration: (values: Record<string, string | null>) =>
    request<Configuration>('PUT', '/ui/api/configuracoes', values),
  section: <N extends SectionName>(name: N) => request<Sections[N]>('GET', `/ui/api/configuracoes/${name}`),
  saveSection: <N extends SectionName>(name: N, value: SectionInput<N>) =>
    request<Sections[N]>('PUT', `/ui/api/configuracoes/${name}`, value),
  indexerStats: (dias: number) =>
    request<{ dias: number; indexadores: IndexerStats[] }>('GET', `/ui/api/indexadores/estatisticas?dias=${dias}`),
  sendToClient: (indexador: string, link: string) =>
    request<SentToClient>('POST', '/ui/api/busca/enviar', { indexador, link }),
  search: (params: { q: string; indexador: string; cat: string }) => {
    const query = new URLSearchParams()
    query.set('q', params.q)
    if (params.indexador) query.set('indexador', params.indexador)
    if (params.cat) query.set('cat', params.cat)
    return request<SearchResponse>('GET', `/ui/api/busca?${query}`)
  },
}

// ---------------------------------------------------------------- biblioteca

export interface LibraryOptions {
  pastas: { caminho: string; livre: number | null }[]
  qualidades: { id: number; nome: string }[]
  indexadores: string[]
}

export interface TmdbResult {
  tmdb: number
  titulo: string
  titulo_original: string
  ano: number | null
  sinopse: string | null
  poster: string | null
  nota: number
  no_catalogo: number | null
}

export interface NewMovie {
  tmdb: number
  pasta: string
  monitorado: boolean
  buscar: boolean
}

export interface MovieChange {
  monitorado?: boolean
  prioritario?: boolean
}

export interface InteractiveRelease {
  guid: string
  titulo: string
  indexador: string
  tamanho: number
  seeders: number | null
  leechers: number | null
  idade_horas: number | null
  qualidade: string | null
  idiomas: string[]
  aprovado: boolean
  outro_filme: boolean
  motivos: string[]
  info: string | null
}

export interface QueueItem {
  /** Filme: o id do grab; série: `serie-<grab>`. */
  id: number | string
  /** Ausente nos itens antigos, que são todos de filme. */
  tipo?: 'filme' | 'serie'
  grab_id?: number
  serie_id?: number
  filme_id: number | null
  filme: string | null
  poster: string | null
  release: string
  indexador: string
  qualidade: string
  tamanho: number
  pego_em: string
  mensagem: string | null
  no_cliente: boolean
  progresso: number | null
  estado_cliente: string | null
  velocidade: number | null
  restante_segundos: number | null
  seeds: number | null
  upgrade: boolean
}

export type HistoryEventKind =
  | 'grabbed'
  | 'imported'
  | 'upgraded'
  | 'failed'
  | 'file_deleted'
  | 'movie_added'
  | 'movie_deleted'
  | 'series_added'
  | 'series_deleted'
  | 'ignored'
  | 'renamed'

export interface HistoryEvent {
  id: number
  movie_id: number | null
  movie_title: string
  event: HistoryEventKind
  at: string
  source_title: string | null
  quality: string | null
  indexer: string | null
  download_id: string | null
  data: Record<string, unknown>
}

export interface BlockedRelease {
  id: number
  movie_id: number | null
  filme: string | null
  source_title: string
  indexer: string | null
  quality: string | null
  size: number | null
  at: string
  message: string | null
}

interface IndexerRules {
  prioridade: number
  seeders_minimos: number
}

export interface DecisionRules {
  tamanho_maximo_mb: number
  aceitar_legenda_embutida: boolean
  legendas_embutidas_liberadas: string
  propers: 'preferir' | 'nao_preferir'
  preferir_flags_do_indexador: boolean
  folga_minima_mb: number
  downloads_simultaneos: number
  carencia_dias: number
  indexadores: Record<string, IndexerRules>
  atraso: { minutos: number; pular_se_melhor_qualidade: boolean }
}

export interface RulesView {
  regras: DecisionRules
  indexadores: string[]
}

export interface NotifyOn {
  pegou: boolean
  importou: boolean
  atualizou: boolean
  falhou: boolean
  removido: boolean
  travou: boolean
  tarefa_falhando: boolean
  indexador_falhando: boolean
  disco_baixo: boolean
  limpeza_abortada: boolean
}

export interface GotifyView {
  servidor: string
  token_definido: boolean
  prioridade: number
  eventos: NotifyOn
  ligado: boolean
}

export interface GotifyInput {
  servidor: string
  /** Vazio mantém o token guardado. */
  token?: string
  prioridade: number
  eventos: NotifyOn
  ligado: boolean
}

const LIB = '/ui/api/biblioteca'

// ---------------------------------------------------------------- renomear e verificar disco

interface RenameStep {
  arquivo_id: number
  de: string
  para: string
  legendas: { de: string; para: string }[]
  feito: boolean
  erro: string | null
}

export interface RenameReport {
  aplicado: boolean
  renomeados: number
  erros: number
  plano: RenameStep[]
}

interface VerifyNew {
  arquivo: string
  tamanho: number
  episodios: number[]
  codigo: string | null
  qualidade: string | null
  estava_dispensado: boolean
  feito: boolean
  erro: string | null
}

interface VerifyGone {
  arquivo_id: number
  arquivo: string
  episodios: number[]
  codigo: string | null
  volta_a_busca: boolean
  feito: boolean
  erro: string | null
}

interface VerifySubtitle {
  arquivo: string
  video: string
  idioma: string | null
  forcada: boolean
  feito: boolean
  erro: string | null
}

export interface VerifyReport {
  aplicado: boolean
  novos: VerifyNew[]
  nao_reconhecidos: { arquivo: string; motivo: string }[]
  sumidos: VerifyGone[]
  legendas: VerifySubtitle[]
}

export interface VerifyAllReport {
  aplicado: boolean
  total: number
  filmes: (VerifyReport & { filme_id: number; filme: string; aviso: string | null })[]
}

export type MediaKind = 'filme' | 'serie'

/** Item de "Faltando": episódio ou filme que ainda não está no disco. */
export type MissingItem =
  | {
      tipo: 'episodio'
      serie_id: number
      serie: string
      poster: string | null
      prioritario: boolean
      episodio_id: number
      temporada: number
      numero: number
      titulo: string | null
      data: string | null
      baixando: boolean
    }
  | {
      tipo: 'filme'
      filme_id: number
      filme: string
      titulo: string
      ano: number | null
      poster: string | null
      prioritario: boolean
      data: string | null
      baixando: boolean
    }

export type ReleaseKind = 'cinema' | 'digital' | 'fisico'

export type CalendarItem =
  | {
      tipo: 'episodio'
      data: string
      serie_id: number
      serie: string
      poster: string | null
      prioritario: boolean
      episodio_id: number
      temporada: number
      numero: number
      titulo: string | null
      estado: 'quero' | 'tenho' | 'dispensado'
      baixando: boolean
      exibido: boolean
    }
  | {
      tipo: 'filme'
      data: string
      lancamento: ReleaseKind
      filme_id: number
      filme: string
      titulo: string
      ano: number | null
      poster: string | null
      prioritario: boolean
      estado: 'tenho' | 'falta' | 'baixando'
    }

const mediaPath = (kind: MediaKind, id: number) => `${LIB}/${kind === 'filme' ? 'filmes' : 'series'}/${id}`

export const library = {
  options: () => request<LibraryOptions>('GET', `${LIB}/opcoes`),
  searchTmdb: (termo: string) =>
    request<{ resultados: TmdbResult[] }>('GET', `${LIB}/tmdb?${new URLSearchParams({ termo })}`),
  add: (movie: NewMovie) => request<{ id: number }>('POST', `${LIB}/filmes`, movie),
  edit: (id: number, change: MovieChange) => request<{ ok: boolean }>('PATCH', `${LIB}/filmes/${id}`, change),
  remove: (id: number, options: { apagar_arquivos: boolean }) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/filmes/${id}?${new URLSearchParams({
        apagar_arquivos: String(options.apagar_arquivos),
      })}`,
    ),
  deleteFile: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/filmes/${id}/arquivo`),
  releases: (id: number) => request<{ releases: InteractiveRelease[] }>('POST', `${LIB}/filmes/${id}/releases`),
  grabRelease: (id: number, guid: string) =>
    request<{ ok: boolean; titulo: string }>('POST', `${LIB}/filmes/${id}/pegar`, { guid }),
  movieHistory: (id: number) =>
    request<{ total: number; eventos: HistoryEvent[] }>('GET', `${LIB}/filmes/${id}/historico?tamanho=50`),
  queue: () => request<{ fila: QueueItem[] }>('GET', `${LIB}/fila`),
  removeDownload: (id: number | string, options: { remover_do_cliente: boolean; bloquear: boolean; buscar: boolean }) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/fila/${id}?${new URLSearchParams({
        remover_do_cliente: String(options.remover_do_cliente),
        bloquear: String(options.bloquear),
        buscar: String(options.buscar),
      })}`,
    ),
  removeSeriesDownload: (
    grabId: number,
    options: { remover_do_cliente: boolean; bloquear: boolean; buscar: boolean },
  ) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/fila/series/${grabId}?${new URLSearchParams({
        remover_do_cliente: String(options.remover_do_cliente),
        bloquear: String(options.bloquear),
        buscar: String(options.buscar),
      })}`,
    ),
  history: (params: { pagina: number; tamanho: number; evento?: string }) => {
    const query = new URLSearchParams({ pagina: String(params.pagina), tamanho: String(params.tamanho) })
    if (params.evento) query.set('evento', params.evento)
    return request<{ total: number; eventos: HistoryEvent[] }>('GET', `${LIB}/historico?${query}`)
  },
  missing: (tipo?: MediaKind) =>
    request<{ total: number; itens: MissingItem[] }>('GET', `${LIB}/faltando${tipo ? `?${new URLSearchParams({ tipo })}` : ''}`),
  calendar: (de: string, ate: string) =>
    request<{ de: string; ate: string; total: number; itens: CalendarItem[] }>(
      'GET',
      `${LIB}/calendario?${new URLSearchParams({ de, ate })}`,
    ),
  rename: (kind: MediaKind, id: number, aplicar: boolean) =>
    request<RenameReport>('POST', `${mediaPath(kind, id)}/renomear?${new URLSearchParams({ aplicar: String(aplicar) })}`),
  verify: (kind: MediaKind, id: number, aplicar: boolean) =>
    request<VerifyReport>('POST', `${mediaPath(kind, id)}/verificar?${new URLSearchParams({ aplicar: String(aplicar) })}`),
  verifyAllMovies: (aplicar: boolean) =>
    request<VerifyAllReport>('POST', `${LIB}/filmes/verificar?${new URLSearchParams({ aplicar: String(aplicar) })}`),
  blocklist: () => request<{ bloqueados: BlockedRelease[] }>('GET', `${LIB}/bloqueados`),
  unblock: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/bloqueados/${id}`),
  rules: () => request<RulesView>('GET', `${LIB}/regras`),
  saveRules: (rules: DecisionRules) => request<{ ok: boolean }>('PUT', `${LIB}/regras`, rules),
  notifications: () => request<{ gotify: GotifyView | null }>('GET', `${LIB}/notificacoes`),
  saveNotifications: (gotify: GotifyInput | null) =>
    request<{ gotify: GotifyView | null }>('PUT', `${LIB}/notificacoes`, gotify),
  testNotification: (gotify: GotifyInput) => request<{ ok: boolean }>('POST', `${LIB}/notificacoes/testar`, gotify),
}

// ---------------------------------------------------------------- séries

interface EpisodeTotals {
  quero: number
  /** Os em Quero que já foram ao ar: o que de fato falta. */
  quero_exibidos: number
  tenho: number
  dispensado: number
  baixando: number
}

export interface SeriesSummary {
  id: number
  tmdb: number
  tvdb: number | null
  imdb: string | null
  titulo: string
  titulo_original: string | null
  titulo_ingles: string | null
  ano: number | null
  status: string | null
  rede: string | null
  poster: string | null
  fundo: string | null
  pasta: string
  pasta_de_temporada: boolean
  monitorar_novos: boolean
  prioritario: boolean
  adicionada: string | null
  atualizada: string | null
  episodios: EpisodeTotals
  tamanho: number
}

type EpisodeState = 'quero' | 'tenho' | 'dispensado'
type SkipReason = 'unwanted' | 'deleted'

interface EpisodeFile {
  id: number
  nome: string
  tamanho: number
  qualidade: string
  idiomas: string[]
  grupo: string | null
  release: string | null
  adicionado: string | null
}

export interface Episode {
  id: number
  numero: number
  titulo: string | null
  /** Dia de exibição, `AAAA-MM-DD`. */
  data: string | null
  exibido: boolean
  sinopse: string | null
  duracao: number | null
  estado: EpisodeState
  baixando: boolean
  motivo: SkipReason | null
  motivo_em: string | null
  qualidade: string | null
  arquivo: EpisodeFile | null
}

export interface Season {
  numero: number
  totais: EpisodeTotals
  episodios: Episode[]
}

interface SeriesDownload {
  id: number
  release: string
  qualidade: string
  tamanho: number
  mensagem: string | null
  pego_em: string
  episodios: string
}

export interface SeriesLastSearch {
  quando: string
  consultas: number
  releases: number
  escolhidos: { titulo: string; indexador: string; qualidade: string; tamanho: number; episodios: string }[]
  motivos: [string, number][]
  erro: string | null
}

export interface SeriesDetail extends SeriesSummary {
  sinopse: string | null
  idioma_original: string | null
  duracao: number | null
  titulos_alternativos: string[]
  temporadas: Season[]
  downloads: SeriesDownload[]
  ultima_busca: SeriesLastSearch | null
}

export type WhatToSearch = 'tudo' | 'ultima_temporada' | 'proximos'

export interface NewSeries {
  tmdb: number
  monitorar_novos: boolean
  pasta_de_temporada: boolean
  buscar: WhatToSearch
  buscar_agora: boolean
}

export interface SeriesChange {
  monitorar_novos?: boolean
  pasta_de_temporada?: boolean
  buscar?: WhatToSearch
  prioritario?: boolean
}

export interface SeriesTmdbResult {
  tmdb: number
  titulo: string
  titulo_original: string
  ano: number | null
  sinopse: string | null
  poster: string | null
  no_catalogo: number | null
}

export interface SeriesRelease {
  guid: string
  titulo: string
  indexador: string
  tamanho: number
  seeders: number | null
  leechers: number | null
  idade_horas: number | null
  qualidade: string | null
  aprovado: boolean
  seria_pego: boolean
  outra_serie: boolean
  /** O que o release cobre, como `S01E01-E10`. */
  episodios: string
  episodio_ids: number[]
  /** Dos cobertos, os que estão em Quero. */
  quero: number[]
  motivos: string[]
  info: string | null
}

export interface SeriesSearchScope {
  temporada?: number
  episodios?: number[]
}

export const seriesApi = {
  list: () => request<{ series: SeriesSummary[] }>('GET', `${LIB}/series`),
  get: (id: number) => request<SeriesDetail>('GET', `${LIB}/series/${id}`),
  searchTmdb: (q: string) =>
    request<{ resultados: SeriesTmdbResult[] }>('GET', `${LIB}/series/buscar-tmdb?${new URLSearchParams({ q })}`),
  add: (series: NewSeries) => request<{ id: number }>('POST', `${LIB}/series`, series),
  edit: (id: number, change: SeriesChange) => request<{ ok: boolean }>('PATCH', `${LIB}/series/${id}`, change),
  remove: (id: number, options: { apagar_arquivos: boolean }) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/series/${id}?${new URLSearchParams({ apagar_arquivos: String(options.apagar_arquivos) })}`,
    ),
  deleteEpisodes: (id: number, ids: number[]) =>
    request<{ ok: boolean; arquivos: number; episodios: number; torrents: number; aviso: string | null }>(
      'POST',
      `${LIB}/series/${id}/episodios/apagar`,
      { ids },
    ),
  skipEpisodes: (id: number, ids: number[], skip: 'unwanted' | null) =>
    request<{ ok: boolean; alterados: number }>('POST', `${LIB}/series/${id}/episodios/skip`, { ids, skip }),
  releases: (id: number, scope: SeriesSearchScope) =>
    request<{ consulta: string; releases: SeriesRelease[] }>('POST', `${LIB}/series/${id}/buscar`, scope),
  grabRelease: (id: number, guid: string) =>
    request<{ ok: boolean; titulo: string; download: unknown }>('POST', `${LIB}/series/${id}/pegar`, { guid }),
  searchNow: (id: number) => request<{ iniciada: boolean }>('POST', `${LIB}/series/${id}/buscar-agora`),
  history: (id: number) =>
    request<{ total: number; eventos: HistoryEvent[] }>('GET', `${LIB}/series/${id}/historico?tamanho=50`),
}

// ---------------------------------------------------------------- para apagar

/** O que se marca para apagar: um filme, uma temporada ou a série inteira. */
export type DeletionTarget = { filme: number } | { serie: number; temporada?: number }

/** Um item marcado (ou sugerido), com o espaço que libera. */
export interface DeletionItem {
  /** Estável: `filme-1`, `serie-2`, `serie-2-t1`. */
  chave: string
  tipo: 'filme' | 'temporada' | 'serie'
  filme?: number
  serie?: number
  temporada?: number
  /** "Título (ano)". */
  titulo: string
  /** "Temporada 2", "Série inteira"; `null` nos filmes. */
  detalhe: string | null
  poster: string | null
  arquivos: number
  /** Bytes dos arquivos que saem. */
  tamanho: number
}

export interface MarkedItem extends DeletionItem {
  marcado_em: string
}

/** O que a regra antiga de assistidos apagaria: alguém assistiu, passada a carência, sem favorito. */
export interface SuggestedItem extends DeletionItem {
  assistido_por: string
  assistido_em: string
}

export interface DeletionSuggestions {
  /** Sem Jellyfin configurado, não há o que sugerir. */
  configurado: boolean
  filmes: SuggestedItem[]
  temporadas: SuggestedItem[]
}

export interface PurgeResult {
  ok: boolean
  apagados: { chave: string; titulo: string; tamanho: number }[]
  falhas: { chave: string; titulo: string; erro: string }[]
  liberado: number
  avisos: string[]
}

/** O alvo de um item da lista, para mandar de volta. */
export function deletionTarget(item: DeletionItem): DeletionTarget {
  if (item.tipo === 'filme') return { filme: item.filme as number }
  if (item.tipo === 'temporada') return { serie: item.serie as number, temporada: item.temporada }
  return { serie: item.serie as number }
}

const PURGE = `${LIB}/para-apagar`

export const deletionApi = {
  list: () => request<{ itens: MarkedItem[]; total: number }>('GET', PURGE),
  suggestions: () => request<DeletionSuggestions>('GET', `${PURGE}/sugestoes`),
  mark: (itens: DeletionTarget[]) => request<{ ok: boolean; marcados: number }>('POST', `${PURGE}/marcar`, { itens }),
  unmark: (itens: DeletionTarget[]) =>
    request<{ ok: boolean; desmarcados: number }>('POST', `${PURGE}/desmarcar`, { itens }),
  purge: (itens: DeletionTarget[]) => request<PurgeResult>('POST', `${PURGE}/apagar`, { itens }),
}

// ---------------------------------------------------------------- descobrir

export type DiscoverList = 'em_alta' | 'populares' | 'em_breve' | 'no_ar'

export interface DiscoverItem {
  tipo: MediaKind
  tmdb: number
  titulo: string
  titulo_original: string
  data: string | null
  ano: number | null
  sinopse: string
  poster: string | null
  nota: number
  popularidade: number
  generos: string[]
}

export interface DiscoverSearchItem extends DiscoverItem {
  no_acervo: boolean
  oculto: boolean
}

export interface DiscoverDetails extends DiscoverSearchItem {
  id_acervo: number | null
  tagline: string
  backdrop: string | null
  duracao: number | null
  temporadas: number | null
  episodios: number | null
  status: string | null
  diretores: string[]
  criadores: string[]
  elenco: { nome: string; personagem: string; foto: string | null }[]
  trailer: string | null
  recomendacoes: DiscoverItem[]
}

export interface DiscoverSearchResponse extends DiscoverPageResponse {
  itens: DiscoverSearchItem[]
}

export interface DiscoverWeek {
  semana: number
  inicio: string
  fim: string
  total: number | null
}

export interface DiscoverWeeks {
  ano: number
  semanas: DiscoverWeek[]
}

export interface DiscoverReleases {
  ano: number
  semana: number
  inicio: string
  fim: string
  itens: DiscoverItem[]
}

export interface DiscoverPageResponse {
  itens: DiscoverItem[]
  pagina: number
  total_paginas: number
}

export interface DiscoverGenre {
  id: number
  nome: string
  oculto: boolean
}

export interface DiscoverHidden {
  titulos: { tipo: MediaKind; tmdb: number; titulo: string; em: string }[]
  semanas: { ano: number; semana: number; em: string }[]
  generos: { id: number; nome: string; em: string }[]
}

const DISCOVER = '/ui/api/descobrir'
export const discover = {
  details: (tipo: MediaKind, tmdb: number) => request<DiscoverDetails>('GET', `${DISCOVER}/titulo/${tipo}/${tmdb}`),
  search: (q: string, pagina: number) =>
    request<DiscoverSearchResponse>('GET', `${DISCOVER}/busca?${new URLSearchParams({ q, pagina: String(pagina) })}`),
  weeks: (ano: number) => request<DiscoverWeeks>('GET', `${DISCOVER}/semanas/${ano}`),
  releases: (ano: number, semana: number) =>
    request<DiscoverReleases>('GET', `${DISCOVER}/semanas/${ano}/${semana}`),
  list: (lista: DiscoverList, tipo: MediaKind, pagina: number) =>
    request<DiscoverPageResponse>('GET', `${DISCOVER}/listas/${lista}?${new URLSearchParams({ tipo, pagina: String(pagina) })}`),
  genres: () => request<{ generos: DiscoverGenre[] }>('GET', `${DISCOVER}/generos`),
  hidden: () => request<DiscoverHidden>('GET', `${DISCOVER}/ocultos`),
  hideTitle: (title: Pick<DiscoverItem, 'tipo' | 'tmdb' | 'titulo'>) =>
    request<{ ok: boolean }>('POST', `${DISCOVER}/ocultos/titulos`, title),
  showTitle: (tipo: MediaKind, tmdb: number) =>
    request<{ ok: boolean; removido: boolean }>('DELETE', `${DISCOVER}/ocultos/titulos/${tipo}/${tmdb}`),
  hideWeek: (ano: number, semana: number) =>
    request<{ ok: boolean }>('POST', `${DISCOVER}/ocultos/semanas`, { ano, semana }),
  showWeek: (ano: number, semana: number) =>
    request<{ ok: boolean; removido: boolean }>('DELETE', `${DISCOVER}/ocultos/semanas/${ano}/${semana}`),
  hideGenre: (id: number, nome: string) =>
    request<{ ok: boolean }>('POST', `${DISCOVER}/ocultos/generos`, { id, nome }),
  showGenre: (id: number) =>
    request<{ ok: boolean; removido: boolean }>('DELETE', `${DISCOVER}/ocultos/generos/${id}`),
}
