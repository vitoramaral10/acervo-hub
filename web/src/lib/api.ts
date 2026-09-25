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
}

export interface Indexer {
  nome: string
  ativo: boolean
  origem: 'config' | 'interface'
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
  modo: 'simulacao' | 'aplicado'
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
  disponibilidade_minima: string | null
  tags: number[]
  do_radarr: boolean
  perfil: string | null
  pasta: string
  adicionado: string | null
  arquivo: MovieFile | null
  sombra: MovieShadow | null
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
  busca_automatica: boolean
}

export interface GrabReport {
  filme: string
  releases: number
  escolhido: { titulo: string; indexador: string; qualidade: string; tamanho: number } | null
  motivos: [string, number][]
  aplicado: boolean
}

export interface MovieShadow {
  quando: string
  releases: number
  pegaria: string | null
  qualidade: string | null
  motivos: [string, number][]
  erro: string | null
}

export interface MovieImport {
  nome: string
  erro: string | null
  resumo: {
    created: string[]
    updated: string[]
    removed: string[]
    unchanged: number
    profiles: number
    applied: boolean
  } | null
}

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
  lastCycle: () => request<{ ultimo: CycleReport | null }>('GET', '/ui/api/limpeza'),
  simulateCycle: () => request<CycleReport>('POST', '/ui/api/limpeza/simular'),
  movies: () => request<{ filmes: Movie[] }>('GET', '/ui/api/filmes'),
  shadow: (limite: number) => request<{ filmes: unknown[] }>('POST', '/ui/api/filmes/sombra', { limite }),
  grab: (id: number, aplicar: boolean) =>
    request<GrabReport>('POST', `/ui/api/filmes/${id}/pegar`, { aplicar }),
  importDownloads: () => request<{ downloads: unknown[] }>('POST', '/ui/api/downloads/importar'),
  configuration: () => request<Configuration>('GET', '/ui/api/configuracoes'),
  saveConfiguration: (values: Record<string, string | null>) =>
    request<Configuration>('PUT', '/ui/api/configuracoes', values),
  importMovies: (aplicar: boolean) =>
    request<{ instancias: MovieImport[] }>('POST', '/ui/api/filmes/importar', { aplicar }),
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
  perfis: { id: number; nome: string }[]
  pastas: { caminho: string; livre: number | null }[]
  tags: { id: number; nome: string }[]
  dono_das_regras: 'radarr' | 'acervo'
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
  excluido: boolean
}

export interface NewMovie {
  tmdb: number
  perfil: string
  pasta: string
  monitorado: boolean
  disponibilidade_minima: string
  tags: number[]
  buscar: boolean
}

export interface MovieChange {
  monitorado?: boolean
  perfil?: string
  disponibilidade_minima?: string
  tags?: number[]
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
  formatos: string[]
  nota: number
  aprovado: boolean
  outro_filme: boolean
  motivos: string[]
  info: string | null
}

export interface QueueItem {
  id: number
  filme_id: number
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

export interface Exclusion {
  tmdb_id: number
  title: string
  year: number | null
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
  pular_checagem_de_espaco: boolean
  carencia_dias: number
  indexadores: Record<string, IndexerRules>
  atraso: { minutos: number; pular_se_melhor_qualidade: boolean; pular_acima_da_nota: number | null }
}

export interface RulesView {
  dono: 'radarr' | 'acervo'
  tem_radarr: boolean
  regras: DecisionRules
  indexadores: string[]
}

export interface ProfileItem {
  nome: string
  qualidades: number[]
  permitido: boolean
}

export interface Profile {
  id: number
  nome: string
  upgrade: boolean
  /** Posição em `itens` do corte. */
  corte: number | null
  idioma: string | null
  /** Do pior ao melhor. */
  itens: ProfileItem[]
  nota_minima: number
  nota_corte: number
  /** Nota por id de formato. */
  notas: Record<string, number>
  em_uso: number
  do_radarr: boolean
}

export type ProfileInput = Omit<Profile, 'id' | 'em_uso' | 'do_radarr'>

export type FormatRule =
  | { tipo: 'titulo'; valor: string }
  | { tipo: 'grupo'; valor: string }
  | { tipo: 'edicao'; valor: string }
  | { tipo: 'idioma'; valor: string }
  | { tipo: 'fonte'; valor: string }
  | { tipo: 'resolucao'; valor: number }
  | { tipo: 'modificador'; valor: string }
  | { tipo: 'tamanho'; minimo: number; maximo: number }
  | { tipo: 'flag'; valor: number }

export type FormatSpec = FormatRule & { nome: string; negar?: boolean; obrigatoria?: boolean }

export interface CustomFormat {
  id: number
  nome: string
  especificacoes: FormatSpec[]
  no_nome_do_arquivo: boolean
}

export type CustomFormatInput = Omit<CustomFormat, 'id'>

export interface FormatTest {
  qualidade: string
  idiomas: string[]
  grupo: string | null
  formatos: { id: number; nome: string }[]
}

export interface SizeDefinition {
  qualidade: number
  nome: string
  minimo: number | null
  maximo: number | null
  preferido: number | null
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

export type ImportListKind = 'tmdb_person' | 'tmdb_collection' | 'tmdb_list'

export interface ImportList {
  id: number
  name: string
  kind: ImportListKind
  /** tmdb_person: {pessoa, elenco, departamentos}; tmdb_collection: {colecao}; tmdb_list: {lista}. */
  settings: Record<string, unknown>
  enabled: boolean
  monitor: boolean
  search_on_add: boolean
  quality_profile_id: number | null
  root_folder: string
  minimum_availability: string
  tags: number[]
  last_sync: string | null
  last_error: string | null
}

export interface ImportListInput {
  nome: string
  tipo: ImportListKind
  configuracao: Record<string, unknown>
  ligada: boolean
  monitorar: boolean
  buscar_ao_adicionar: boolean
  perfil: number
  pasta: string
  disponibilidade_minima: string
  tags: number[]
}

export interface ListSyncReport {
  lista: string
  encontrados: number
  adicionados: string[]
  ja_no_catalogo: number
  excluidos: number
  sem_data: number
  falhas: [string, string][]
}

export interface ListPreviewMovie {
  tmdb: number
  titulo: string
  ano: number | null
  poster: string | null
  no_catalogo: boolean
  excluido: boolean
}

export interface MigrationReport {
  historico: number
  historico_ja_migrado: boolean
  bloqueados: number
  exclusoes: number
  notificacao: boolean
  listas: string[]
  avisos: string[]
}

const LIB = '/ui/api/biblioteca'

export const library = {
  options: () => request<LibraryOptions>('GET', `${LIB}/opcoes`),
  searchTmdb: (termo: string) =>
    request<{ resultados: TmdbResult[] }>('GET', `${LIB}/tmdb?${new URLSearchParams({ termo })}`),
  add: (movie: NewMovie) => request<{ id: number }>('POST', `${LIB}/filmes`, movie),
  edit: (id: number, change: MovieChange) => request<{ ok: boolean }>('PATCH', `${LIB}/filmes/${id}`, change),
  remove: (id: number, options: { apagar_arquivos: boolean; excluir: boolean }) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/filmes/${id}?${new URLSearchParams({
        apagar_arquivos: String(options.apagar_arquivos),
        excluir: String(options.excluir),
      })}`,
    ),
  deleteFile: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/filmes/${id}/arquivo`),
  releases: (id: number) => request<{ releases: InteractiveRelease[] }>('POST', `${LIB}/filmes/${id}/releases`),
  grabRelease: (id: number, guid: string) =>
    request<{ ok: boolean; titulo: string }>('POST', `${LIB}/filmes/${id}/pegar`, { guid }),
  movieHistory: (id: number) =>
    request<{ total: number; eventos: HistoryEvent[] }>('GET', `${LIB}/filmes/${id}/historico?tamanho=50`),
  queue: () => request<{ fila: QueueItem[] }>('GET', `${LIB}/fila`),
  removeDownload: (id: number, options: { remover_do_cliente: boolean; bloquear: boolean; buscar: boolean }) =>
    request<{ ok: boolean }>(
      'DELETE',
      `${LIB}/fila/${id}?${new URLSearchParams({
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
  exclusions: () => request<{ exclusoes: Exclusion[] }>('GET', `${LIB}/exclusoes`),
  addExclusion: (exclusion: { tmdb: number; titulo: string; ano: number | null }) =>
    request<{ ok: boolean }>('POST', `${LIB}/exclusoes`, exclusion),
  removeExclusion: (tmdb: number) => request<{ ok: boolean }>('DELETE', `${LIB}/exclusoes/${tmdb}`),
  rules: () => request<RulesView>('GET', `${LIB}/regras`),
  saveRules: (rules: DecisionRules) => request<{ ok: boolean }>('PUT', `${LIB}/regras`, rules),
  takeOver: () => request<{ dono: string }>('POST', `${LIB}/regras/assumir`),
  giveBack: () => request<{ dono: string }>('POST', `${LIB}/regras/devolver`),
  profiles: () => request<{ perfis: Profile[] }>('GET', `${LIB}/perfis`),
  createProfile: (profile: ProfileInput) => request<{ id: number }>('POST', `${LIB}/perfis`, profile),
  updateProfile: (id: number, profile: ProfileInput) => request<{ id: number }>('PUT', `${LIB}/perfis/${id}`, profile),
  deleteProfile: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/perfis/${id}`),
  formats: () => request<{ formatos: CustomFormat[] }>('GET', `${LIB}/formatos`),
  createFormat: (format: CustomFormatInput) => request<{ id: number }>('POST', `${LIB}/formatos`, format),
  updateFormat: (id: number, format: CustomFormatInput) =>
    request<{ id: number }>('PUT', `${LIB}/formatos/${id}`, format),
  deleteFormat: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/formatos/${id}`),
  testFormats: (titulo: string) => request<FormatTest>('POST', `${LIB}/formatos/testar`, { titulo }),
  sizes: () => request<{ tamanhos: SizeDefinition[] }>('GET', `${LIB}/tamanhos`),
  saveSizes: (sizes: Pick<SizeDefinition, 'qualidade' | 'minimo' | 'maximo' | 'preferido'>[]) =>
    request<{ ok: boolean }>('PUT', `${LIB}/tamanhos`, sizes),
  notifications: () => request<{ gotify: GotifyView | null }>('GET', `${LIB}/notificacoes`),
  saveNotifications: (gotify: GotifyInput | null) =>
    request<{ gotify: GotifyView | null }>('PUT', `${LIB}/notificacoes`, gotify),
  testNotification: (gotify: GotifyInput) => request<{ ok: boolean }>('POST', `${LIB}/notificacoes/testar`, gotify),
  lists: () => request<{ listas: ImportList[] }>('GET', `${LIB}/listas`),
  createList: (list: ImportListInput) => request<{ id: number }>('POST', `${LIB}/listas`, list),
  updateList: (id: number, list: ImportListInput) => request<{ id: number }>('PUT', `${LIB}/listas/${id}`, list),
  deleteList: (id: number) => request<{ ok: boolean }>('DELETE', `${LIB}/listas/${id}`),
  syncList: (id: number) => request<ListSyncReport>('POST', `${LIB}/listas/${id}/sincronizar`),
  previewList: (id: number) => request<{ filmes: ListPreviewMovie[] }>('GET', `${LIB}/listas/${id}/previa`),
  migrate: () => request<MigrationReport>('POST', `${LIB}/migrar`),
}
