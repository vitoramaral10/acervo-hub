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
