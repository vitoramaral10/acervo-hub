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

export const api = {
  session: () => request<{ ok: boolean }>('GET', '/ui/api/sessao'),
  login: (chave: string) => request<{ ok: boolean }>('POST', '/ui/api/entrar', { chave }),
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
  search: (params: { q: string; indexador: string; cat: string }) => {
    const query = new URLSearchParams()
    query.set('q', params.q)
    if (params.indexador) query.set('indexador', params.indexador)
    if (params.cat) query.set('cat', params.cat)
    return request<SearchResponse>('GET', `/ui/api/busca?${query}`)
  },
}
