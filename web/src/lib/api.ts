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

export interface Health {
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

export interface SyncAction {
  acao: 'criar' | 'atualizar' | 'remover' | 'manter'
  indexador: string
  categorias: number[]
  falha: string | null
}

export interface SyncReport {
  aplicado: boolean
  falhas: number
  instancias: { nome: string; tipo: string; erro: string | null; acoes: SyncAction[] }[]
}

export interface Apps {
  endereco_publico: string | null
  instancias: { nome: string; tipo: 'series' | 'filmes'; url: string }[]
}

export interface CycleReport {
  quando: string
  instancias: { nome: string; fila: number | null; obras: number | null; erro: string | null }[]
  torrents: number
  ilegiveis: { nome: string; motivo: string }[]
  biblioteca: string
  abortado: string | null
  acoes: { tipo: string; instancia: string | null; titulo: string; detalhe: string }[]
  espaco: string
  pulados: { motivo: string; quantos: number }[]
  executadas: number | null
  falharam: number | null
}

/** O detalhe de "Apagar assistidos": o que saiu do acervo e o que ficou, com o motivo. */
export interface WatchedReport {
  apagados: { titulo: string; ano: number | null; assistido_por: string; assistido_em: string; tamanho: string }[]
  pulados: { titulo: string; ano: number | null; motivo: string }[]
  recusados: number
  liberado: string
  aviso: string | null
}

/** Como uma execução de tarefa terminou. */
export interface TaskLastRun {
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

/** Uma execução no histórico; `detalhe` é o relatório dela (limpeza: `CycleReport`; assistidos: `WatchedReport`; metadados: `MetadataReport`). */
export interface TaskRun extends TaskLastRun {
  id: number
  tarefa: string
  nome: string
  detalhe: unknown
}

export interface MovieFile {
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
export interface Secret {
  definida: boolean
}

export interface ServerSection {
  api_key: Secret
  public_url: string | null
  catalogos: string[]
  http_timeout_seconds: number
}

export interface DownloadClientSection {
  url: string
  username: string
  password: Secret
}

export interface JellyfinSection {
  url: string
  api_key: Secret
  delete_watched_after_minutes: number
}

export interface Manager {
  name: string
  kind: 'series' | 'movie'
  url: string
  api_key: Secret
}

export interface LibrarySection {
  roots: string[]
  root_folders: string[]
  category: string
  paths: Record<string, string>
}

export interface CleanupSection {
  orphan_strikes: number
  delete_private_orphans: boolean
  skip_orphan_if_missing_in_client: boolean
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
  gerenciadores: Manager[]
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
  apps: () => request<{ aplicativos: Apps }>('GET', '/ui/api/aplicativos'),
  sync: (aplicar: boolean) => request<SyncReport>('POST', '/ui/api/aplicativos/sincronizar', { aplicar }),
  tasks: () => request<{ tarefas: Task[] }>('GET', '/ui/api/tarefas'),
  taskHistory: () => request<{ historico: TaskRun[] }>('GET', '/ui/api/tarefas/historico'),
  runTask: (id: string) =>
    request<{ iniciada: boolean; tarefa: Task | null }>('POST', `/ui/api/tarefas/${encodeURIComponent(id)}/rodar`),
  movies: () => request<{ filmes: Movie[] }>('GET', '/ui/api/filmes'),
  searchMissing: () => request<MissingSearch>('POST', '/ui/api/filmes/buscar'),
  missingSearch: () => request<MissingSearch>('GET', '/ui/api/filmes/buscar'),
  grab: (id: number, aplicar: boolean) =>
    request<GrabReport>('POST', `/ui/api/filmes/${id}/pegar`, { aplicar }),
  importDownloads: () => request<{ downloads: unknown[] }>('POST', '/ui/api/downloads/importar'),
  configuration: () => request<Configuration>('GET', '/ui/api/configuracoes'),
  saveConfiguration: (values: Record<string, string | null>) =>
    request<Configuration>('PUT', '/ui/api/configuracoes', values),
  section: <N extends SectionName>(name: N) => request<Sections[N]>('GET', `/ui/api/configuracoes/${name}`),
  saveSection: <N extends SectionName>(name: N, value: SectionInput<N>) =>
    request<Sections[N]>('PUT', `/ui/api/configuracoes/${name}`, value),
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

export interface IndexerRules {
  prioridade: number
  seeders_minimos: number
}

export interface DecisionRules {
  tamanho_maximo_mb: number
  aceitar_legenda_embutida: boolean
  legendas_embutidas_liberadas: string
  propers: 'preferir_e_atualizar' | 'nao_atualizar' | 'nao_preferir'
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

export interface EpisodeTotals {
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
  adicionada: string | null
  atualizada: string | null
  episodios: EpisodeTotals
  tamanho: number
}

export type EpisodeState = 'quero' | 'tenho' | 'dispensado'
export type SkipReason = 'unwanted' | 'deleted' | 'watched'

export interface EpisodeFile {
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

export interface SeriesDownload {
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
