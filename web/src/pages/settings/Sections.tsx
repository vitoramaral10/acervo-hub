// As seções da configuração do serviço, cada uma gravada no banco por
// `/ui/api/configuracoes/{secao}`. Segredo nunca volta da API: o campo vem
// vazio, com "Definida" no placeholder, e salvar em branco mantém o guardado.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, CircleDashed, KeyRound, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge, Skeleton } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import {
  type CleanupSection,
  type DownloadClientSection,
  type JellyfinSection,
  type LibrarySection,
  type Manager,
  type SectionInput,
  type SectionName,
  type Sections,
  type ServerSection,
  api,
} from '@/lib/api'
import { Field, Toggle } from '@/pages/settings/Rules'

export const sectionKey = (name: SectionName) => ['configuracao', name] as const

/** Lê e grava uma seção; a resposta do PUT já é a seção como ficou. */
export function useSection<N extends SectionName>(name: N) {
  const queryClient = useQueryClient()
  const query = useQuery({ queryKey: sectionKey(name), queryFn: () => api.section(name) })
  const save = useMutation({
    mutationFn: (value: SectionInput<N>) => api.saveSection(name, value),
    onSuccess: (data) => {
      queryClient.setQueryData(sectionKey(name), data)
      // Tarefas (disponíveis ou não) e aplicativos dependem da configuração.
      for (const key of ['tarefas', 'aplicativos', 'sincronizacao']) {
        void queryClient.invalidateQueries({ queryKey: [key] })
      }
      toast.success('Configuração salva')
    },
    onError: (error: Error) => toast.error(error.message),
  })
  return { query, save }
}

type FormProps<N extends SectionName> = {
  data: Sections[N]
  save: (value: SectionInput<N>) => void
  saving: boolean
}

/** Moldura de uma seção: título, estado de carga e erro, e o formulário. */
function SectionCard<N extends SectionName>({
  name,
  title,
  description,
  status,
  Form,
}: {
  name: N
  title: string
  description: string
  status?: (data: Sections[N]) => boolean
  Form: (props: FormProps<N>) => ReactNode
}) {
  const { query, save } = useSection(name)
  const id = `secao-${name}`
  return (
    <section aria-labelledby={id} className="max-w-2xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id={id} className="text-lg font-semibold">
            {title}
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">{description}</p>
        </div>
        {status && query.data && (
          <Badge tone={status(query.data) ? 'success' : undefined}>
            {status(query.data) ? <CircleCheck aria-hidden="true" /> : <CircleDashed aria-hidden="true" />}
            {status(query.data) ? 'Configurado' : 'Não configurado'}
          </Badge>
        )}
      </div>
      {query.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{query.error.message}</p>
          <Button onClick={() => void query.refetch()}>Tentar novamente</Button>
        </div>
      ) : query.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-4">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-9 w-32" />
        </div>
      ) : (
        // Remonta a cada gravação: o formulário volta ao que ficou salvo, com os segredos em branco.
        <Form key={query.dataUpdatedAt} data={query.data} save={(value) => save.mutate(value)} saving={save.isPending} />
      )}
    </section>
  )
}

function SecretInput({
  id,
  defined,
  value,
  onChange,
  describedBy,
}: {
  id: string
  defined: boolean
  value: string
  onChange: (value: string) => void
  describedBy?: string
}) {
  return (
    <div className="relative">
      <KeyRound
        className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-content-subtle"
        aria-hidden="true"
      />
      <Input
        id={id}
        type="password"
        autoComplete="off"
        spellCheck={false}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={defined ? 'Definida — preencha só para trocar' : ''}
        aria-describedby={describedBy}
        className="pl-9 font-mono"
      />
    </div>
  )
}

/** Lista de textos, um por linha. */
function LinesInput({
  id,
  value,
  onChange,
  placeholder,
  describedBy,
}: {
  id: string
  value: string
  onChange: (value: string) => void
  placeholder?: string
  describedBy?: string
}) {
  return (
    <textarea
      id={id}
      rows={3}
      spellCheck={false}
      value={value}
      placeholder={placeholder}
      aria-describedby={describedBy}
      onChange={(event) => onChange(event.target.value)}
      className="w-full min-w-0 rounded-md border border-border-strong bg-surface px-3 py-2 font-mono text-sm text-content shadow-sm transition-colors outline-none placeholder:text-content-subtle focus-visible:border-accent focus-visible:ring-3 focus-visible:ring-ring/25"
    />
  )
}

const lines = (text: string) =>
  text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)

const whole = (value: string) => (value.trim() === '' ? 0 : Math.max(0, Math.trunc(Number(value)) || 0))

function SaveRow({ saving, children }: { saving: boolean; children?: ReactNode }) {
  return (
    <div className="flex flex-wrap gap-2 border-t border-border pt-4">
      <Button type="submit" variant="primary" loading={saving}>
        Salvar
      </Button>
      {children}
    </div>
  )
}

// ---------------------------------------------------------------- servidor

/** 32 caracteres hexadecimais do gerador do navegador. */
function newKey(): string {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
}

function ServerForm({ data, save, saving }: FormProps<'servidor'>) {
  const [key, setKey] = useState('')
  const [generated, setGenerated] = useState(false)
  const [publicUrl, setPublicUrl] = useState(data.public_url ?? '')
  const [catalogs, setCatalogs] = useState(data.catalogos.join('\n'))
  const [reserve, setReserve] = useState(data.catalogos_reserva.join('\n'))
  const [definitionsUrl, setDefinitionsUrl] = useState(data.definicoes_url)
  const [timeout, setTimeoutSeconds] = useState(String(data.http_timeout_seconds))
  const [xemUrl, setXemUrl] = useState(data.xem_url)
  const [flaresolverrUrl, setFlaresolverrUrl] = useState(data.flaresolverr_url ?? '')
  const [flaresolverrTimeout, setFlaresolverrTimeout] = useState(String(data.flaresolverr_timeout_s))
  const [proxyUrl, setProxyUrl] = useState(data.proxy_url ?? '')
  const [proxyUser, setProxyUser] = useState(data.proxy_username)
  const [proxyPassword, setProxyPassword] = useState('')
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        const value: SectionInput<'servidor'> = {
          public_url: publicUrl.trim() || null,
          catalogos: lines(catalogs),
          catalogos_reserva: lines(reserve),
          definicoes_url: definitionsUrl.trim(),
          http_timeout_seconds: whole(timeout),
          xem_url: xemUrl.trim(),
          flaresolverr_url: flaresolverrUrl.trim() || null,
          flaresolverr_timeout_s: whole(flaresolverrTimeout),
          proxy_url: proxyUrl.trim() || null,
          proxy_username: proxyUser.trim(),
        }
        if (key.trim()) value.api_key = key.trim()
        if (proxyPassword) value.proxy_password = proxyPassword
        save(value)
      }}
    >
      <Field
        id="servidor-chave"
        label="Chave de API"
        help="Vale para o Torznab e o cabeçalho X-Api-Key. Ao menos 16 caracteres. Trocar derruba quem usa a antiga: sincronize os aplicativos depois."
      >
        {generated ? (
          <Input
            id="servidor-chave"
            readOnly
            value={key}
            onFocus={(event) => event.currentTarget.select()}
            aria-describedby="servidor-chave-ajuda servidor-chave-nova"
            className="font-mono"
          />
        ) : (
          <SecretInput
            id="servidor-chave"
            defined={data.api_key.definida}
            value={key}
            onChange={setKey}
            describedBy="servidor-chave-ajuda"
          />
        )}
      </Field>
      {generated && (
        <p id="servidor-chave-nova" role="status" className="text-sm text-warning">
          Chave nova gerada. Copie agora — depois de salva ela não aparece mais. Só vale ao salvar.
        </p>
      )}
      <div>
        <Button
          type="button"
          size="sm"
          onClick={() => {
            setKey(newKey())
            setGenerated(true)
          }}
        >
          <RefreshCw aria-hidden="true" />
          Gerar nova chave
        </Button>
      </div>
      <Field
        id="servidor-endereco"
        label="Endereço público"
        help="Como os gerenciadores alcançam este serviço; é o que a sincronização cadastra neles. Em Compose, o nome do serviço na rede interna."
      >
        <Input
          id="servidor-endereco"
          type="url"
          spellCheck={false}
          value={publicUrl}
          onChange={(event) => setPublicUrl(event.target.value)}
          placeholder="http://acervo-hub:9797"
          aria-describedby="servidor-endereco-ajuda"
        />
      </Field>
      <Field
        id="servidor-catalogos"
        label="Diretórios locais de definições Cardigann"
        help="Um por linha: as definições customizadas. Valem acima de tudo e nunca são trocadas pelas do repositório; entre eles, o primeiro que tiver um id vence."
      >
        <LinesInput
          id="servidor-catalogos"
          value={catalogs}
          onChange={setCatalogs}
          placeholder="/etc/acervo-hub/definicoes"
          describedBy="servidor-catalogos-ajuda"
        />
      </Field>
      <Field
        id="servidor-reserva"
        label="Diretórios de reserva (catálogo antigo)"
        help="Um por linha. Valem abaixo das definições que a tarefa «Atualização das definições» baixa do repositório oficial; servem enquanto ela não rodou."
      >
        <LinesInput
          id="servidor-reserva"
          value={reserve}
          onChange={setReserve}
          placeholder="/etc/acervo-hub/catalogo"
          describedBy="servidor-reserva-ajuda"
        />
      </Field>
      <Field
        id="servidor-definicoes"
        label="Arquivo do repositório de definições"
        help="O .tar.gz da branch; a tarefa lê as definições de definitions/v11 dele."
      >
        <Input
          id="servidor-definicoes"
          type="url"
          spellCheck={false}
          value={definitionsUrl}
          onChange={(event) => setDefinitionsUrl(event.target.value)}
          aria-describedby="servidor-definicoes-ajuda"
        />
      </Field>
      <Field
        id="servidor-flaresolverr"
        label="FlareSolverr"
        help="Vence o desafio do Cloudflare (resposta 403 ou 503 com «Just a moment...»). Em branco, o desafio vira erro. Cada indexador escolhe se o usa."
      >
        <Input
          id="servidor-flaresolverr"
          type="url"
          spellCheck={false}
          value={flaresolverrUrl}
          onChange={(event) => setFlaresolverrUrl(event.target.value)}
          placeholder="http://flaresolverr:8191"
          aria-describedby="servidor-flaresolverr-ajuda"
        />
      </Field>
      <Field
        id="servidor-flaresolverr-timeout"
        label="Timeout do FlareSolverr (segundos)"
        help="Quanto ele pode levar para vencer um desafio."
      >
        <Input
          id="servidor-flaresolverr-timeout"
          type="number"
          min={1}
          max={300}
          value={flaresolverrTimeout}
          onChange={(event) => setFlaresolverrTimeout(event.target.value)}
          aria-describedby="servidor-flaresolverr-timeout-ajuda"
          className="w-28 tabular-nums"
        />
      </Field>
      <Field
        id="servidor-proxy"
        label="Proxy dos indexadores"
        help="http://, https:// ou socks5://, sem usuário e senha na URL. Só os indexadores marcados para usá-lo saem por ele — busca, login e download."
      >
        <Input
          id="servidor-proxy"
          type="url"
          spellCheck={false}
          value={proxyUrl}
          onChange={(event) => setProxyUrl(event.target.value)}
          placeholder="socks5://proxy:1080"
          aria-describedby="servidor-proxy-ajuda"
        />
      </Field>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field id="servidor-proxy-usuario" label="Usuário do proxy" help="Em branco, sem autenticação.">
          <Input
            id="servidor-proxy-usuario"
            spellCheck={false}
            autoComplete="off"
            value={proxyUser}
            onChange={(event) => setProxyUser(event.target.value)}
            aria-describedby="servidor-proxy-usuario-ajuda"
          />
        </Field>
        <Field id="servidor-proxy-senha" label="Senha do proxy" help="Em branco mantém a guardada.">
          <SecretInput
            id="servidor-proxy-senha"
            defined={data.proxy_password.definida}
            value={proxyPassword}
            onChange={setProxyPassword}
            describedBy="servidor-proxy-senha-ajuda"
          />
        </Field>
      </div>
      <Field id="servidor-timeout" label="Timeout HTTP (segundos)" help="De cada chamada a tracker, cliente e gerenciador.">
        <Input
          id="servidor-timeout"
          type="number"
          min={1}
          max={600}
          value={timeout}
          onChange={(event) => setTimeoutSeconds(event.target.value)}
          aria-describedby="servidor-timeout-ajuda"
          className="w-28 tabular-nums"
        />
      </Field>
      <Field
        id="servidor-xem"
        label="Endereço do XEM"
        help="De onde vem a numeração de cena das séries (a tarefa «Numeração de cena»). O padrão é https://thexem.info."
      >
        <Input
          id="servidor-xem"
          type="url"
          spellCheck={false}
          value={xemUrl}
          onChange={(event) => setXemUrl(event.target.value)}
          placeholder="https://thexem.info"
          aria-describedby="servidor-xem-ajuda"
        />
      </Field>
      <SaveRow saving={saving} />
    </form>
  )
}

export function ServerSettings() {
  return (
    <SectionCard
      name="servidor"
      title="Servidor"
      description="A chave da superfície Torznab, o endereço pelo qual os gerenciadores chegam aqui e o catálogo de definições."
      status={(data: ServerSection) => data.api_key.definida}
      Form={ServerForm}
    />
  )
}

// ---------------------------------------------------------------- cliente de download

function DownloadClientForm({ data, save, saving }: FormProps<'qbittorrent'>) {
  const [url, setUrl] = useState(data.url)
  const [username, setUsername] = useState(data.username)
  const [password, setPassword] = useState('')
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        const value: SectionInput<'qbittorrent'> = { url: url.trim(), username: username.trim() }
        if (password) value.password = password
        save(value)
      }}
    >
      <Field id="qbit-url" label="URL" help="Em branco, não há cliente: o grab, a importação e a limpeza ficam parados.">
        <Input
          id="qbit-url"
          type="url"
          spellCheck={false}
          value={url}
          onChange={(event) => setUrl(event.target.value)}
          placeholder="http://qbittorrent:8080"
          aria-describedby="qbit-url-ajuda"
        />
      </Field>
      <Field id="qbit-usuario" label="Usuário">
        <Input
          id="qbit-usuario"
          autoComplete="off"
          spellCheck={false}
          value={username}
          onChange={(event) => setUsername(event.target.value)}
        />
      </Field>
      <Field id="qbit-senha" label="Senha">
        <SecretInput id="qbit-senha" defined={data.password.definida} value={password} onChange={setPassword} />
      </Field>
      <SaveRow saving={saving} />
    </form>
  )
}

export function DownloadClientSettings() {
  return (
    <SectionCard
      name="qbittorrent"
      title="qBittorrent"
      description="Para onde vão os torrents que o acervo pega, e por onde a limpeza age."
      status={(data: DownloadClientSection) => data.url !== ''}
      Form={DownloadClientForm}
    />
  )
}

// ---------------------------------------------------------------- jellyfin

function JellyfinForm({ data, save, saving }: FormProps<'jellyfin'>) {
  const [url, setUrl] = useState(data.url)
  const [key, setKey] = useState('')
  const [grace, setGrace] = useState(String(data.delete_watched_after_minutes))
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        const value: SectionInput<'jellyfin'> = { url: url.trim(), delete_watched_after_minutes: whole(grace) }
        if (key.trim()) value.api_key = key.trim()
        save(value)
      }}
    >
      <Field id="jellyfin-url" label="URL" help="Em branco, a tarefa de apagar assistidos fica parada.">
        <Input
          id="jellyfin-url"
          type="url"
          spellCheck={false}
          value={url}
          onChange={(event) => setUrl(event.target.value)}
          placeholder="http://jellyfin:8096"
          aria-describedby="jellyfin-url-ajuda"
        />
      </Field>
      <Field
        id="jellyfin-chave"
        label="Chave de API"
        help="Painel → Chaves de API. A varredura da biblioteca, pedida depois de apagar, exige chave de administrador."
      >
        <SecretInput
          id="jellyfin-chave"
          defined={data.api_key.definida}
          value={key}
          onChange={setKey}
          describedBy="jellyfin-chave-ajuda"
        />
      </Field>
      <Field
        id="jellyfin-carencia"
        label="Carência depois de assistido (minutos)"
        help="Dá tempo de marcar como favorito o que é para ficar. Favorito de qualquer usuário nunca sai."
      >
        <Input
          id="jellyfin-carencia"
          type="number"
          min={0}
          value={grace}
          onChange={(event) => setGrace(event.target.value)}
          aria-describedby="jellyfin-carencia-ajuda"
          className="w-28 tabular-nums"
        />
      </Field>
      <SaveRow saving={saving} />
    </form>
  )
}

export function JellyfinSettings() {
  return (
    <SectionCard
      name="jellyfin"
      title="Jellyfin"
      description="Diz o que já foi assistido. Com ele, a tarefa “Apagar assistidos” remove do acervo — pasta e download — o filme visto há mais que a carência."
      status={(data: JellyfinSection) => data.url !== ''}
      Form={JellyfinForm}
    />
  )
}

// ---------------------------------------------------------------- gerenciadores

type ManagerRow = { name: string; kind: Manager['kind']; url: string; key: string; defined: boolean }

function ManagersForm({ data, save, saving }: FormProps<'gerenciadores'>) {
  const [rows, setRows] = useState<ManagerRow[]>(
    data.map((m) => ({ name: m.name, kind: m.kind, url: m.url, key: '', defined: m.api_key.definida })),
  )
  const update = (index: number, patch: Partial<ManagerRow>) =>
    setRows((current) => current.map((row, i) => (i === index ? { ...row, ...patch } : row)))
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        save(
          rows.map((row) => ({
            name: row.name.trim(),
            kind: row.kind,
            url: row.url.trim(),
            ...(row.key.trim() ? { api_key: row.key.trim() } : {}),
          })),
        )
      }}
    >
      {rows.length === 0 && (
        <p className="text-sm text-content-muted">Nenhum gerenciador. Adicione o Sonarr ou o Radarr abaixo.</p>
      )}
      {rows.map((row, index) => {
        const id = `gerenciador-${index}`
        return (
          <fieldset key={id} className="grid gap-3 rounded-md border border-border p-4 sm:grid-cols-2">
            <legend className="px-1 text-sm font-semibold">{row.name || 'Novo gerenciador'}</legend>
            <Field id={`${id}-nome`} label="Nome">
              <Input
                id={`${id}-nome`}
                value={row.name}
                spellCheck={false}
                onChange={(event) => update(index, { name: event.target.value })}
              />
            </Field>
            <Field id={`${id}-tipo`} label="Tipo">
              <Select value={row.kind} onValueChange={(kind) => update(index, { kind: kind as Manager['kind'] })}>
                <SelectTrigger id={`${id}-tipo`}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="series">Séries (Sonarr)</SelectItem>
                  <SelectItem value="movie">Filmes (Radarr)</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field id={`${id}-url`} label="URL">
              <Input
                id={`${id}-url`}
                type="url"
                spellCheck={false}
                value={row.url}
                placeholder="http://sonarr:8989"
                onChange={(event) => update(index, { url: event.target.value })}
              />
            </Field>
            <Field id={`${id}-chave`} label="Chave de API">
              <SecretInput
                id={`${id}-chave`}
                defined={row.defined}
                value={row.key}
                onChange={(key) => update(index, { key })}
              />
            </Field>
            <div className="sm:col-span-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setRows((current) => current.filter((_, i) => i !== index))}
              >
                <Trash2 aria-hidden="true" />
                Remover {row.name || 'este'}
              </Button>
            </div>
          </fieldset>
        )
      })}
      <div>
        <Button
          type="button"
          size="sm"
          onClick={() => setRows((current) => [...current, { name: '', kind: 'series', url: '', key: '', defined: false }])}
        >
          <Plus aria-hidden="true" />
          Adicionar gerenciador
        </Button>
      </div>
      <p className="text-xs text-content-subtle">
        A chave guardada segue o nome: trocar o nome de um gerenciador pede a chave de novo.
      </p>
      <SaveRow saving={saving} />
    </form>
  )
}

export function ManagersSettings() {
  return (
    <SectionCard
      name="gerenciadores"
      title="Gerenciadores"
      description="Os gerenciadores de série e de filme: a sincronização cadastra os indexadores neles, e a limpeza cruza a fila de cada um."
      Form={ManagersForm}
    />
  )
}

// ---------------------------------------------------------------- biblioteca

function LibraryForm({ data, save, saving }: FormProps<'biblioteca'>) {
  const [roots, setRoots] = useState(data.roots.join('\n'))
  const [folders, setFolders] = useState(data.root_folders.join('\n'))
  const [category, setCategory] = useState(data.category)
  const [manualCategory, setManualCategory] = useState(data.categoria_manual)
  const [paths, setPaths] = useState(Object.entries(data.paths))
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        save({
          roots: lines(roots),
          root_folders: lines(folders),
          category: category.trim(),
          categoria_manual: manualCategory.trim(),
          paths: Object.fromEntries(
            paths.map(([from, to]) => [from.trim(), to.trim()]).filter(([from, to]) => from && to),
          ),
        })
      }}
    >
      <Field
        id="biblioteca-pastas"
        label="Pastas raiz dos filmes"
        help="Uma por linha, como o cliente de download as vê. São as opções ao adicionar um filme."
      >
        <LinesInput
          id="biblioteca-pastas"
          value={folders}
          onChange={setFolders}
          placeholder="/media/movies"
          describedBy="biblioteca-pastas-ajuda"
        />
      </Field>
      <Field
        id="biblioteca-raizes"
        label="Raízes do acervo (no host)"
        help="Uma por linha. Medem o tamanho da biblioteca, que é a base da trava proporcional da limpeza."
      >
        <LinesInput
          id="biblioteca-raizes"
          value={roots}
          onChange={setRoots}
          placeholder="/mnt/acervo/movies"
          describedBy="biblioteca-raizes-ajuda"
        />
      </Field>
      <Field
        id="biblioteca-categoria"
        label="Categoria no cliente de download"
        help="Para o que o acervo pega, separada da do gerenciador. Ponha-a também nas categorias gerenciadas da limpeza."
      >
        <Input
          id="biblioteca-categoria"
          spellCheck={false}
          value={category}
          onChange={(event) => setCategory(event.target.value)}
          aria-describedby="biblioteca-categoria-ajuda"
          className="max-w-60"
        />
      </Field>
      <Field
        id="biblioteca-categoria-manual"
        label="Categoria da busca manual"
        help="Para o que você manda ao cliente pela tela Busca. Fica fora das categorias gerenciadas da limpeza: ela nunca apaga esses torrents, e nenhum grab do acervo os adota."
      >
        <Input
          id="biblioteca-categoria-manual"
          spellCheck={false}
          value={manualCategory}
          onChange={(event) => setManualCategory(event.target.value)}
          aria-describedby="biblioteca-categoria-manual-ajuda"
          className="max-w-60"
        />
      </Field>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-sm font-medium">Caminhos</legend>
        <p className="text-xs text-content-subtle">
          Caminho como o cliente de download o vê → caminho onde o acervo-hub o lê. Sem isso, o acervo procuraria
          o arquivo no lugar errado.
        </p>
        {paths.map(([from, to], index) => (
          <div key={index} className="flex items-center gap-2">
            <Input
              aria-label={`Caminho no cliente, linha ${index + 1}`}
              spellCheck={false}
              value={from}
              placeholder="/media"
              onChange={(event) =>
                setPaths((current) => current.map((pair, i) => (i === index ? [event.target.value, pair[1]] : pair)))
              }
              className="font-mono"
            />
            <span aria-hidden="true" className="text-content-subtle">
              →
            </span>
            <Input
              aria-label={`Caminho aqui, linha ${index + 1}`}
              spellCheck={false}
              value={to}
              placeholder="/mnt/acervo"
              onChange={(event) =>
                setPaths((current) => current.map((pair, i) => (i === index ? [pair[0], event.target.value] : pair)))
              }
              className="font-mono"
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Remover o caminho da linha ${index + 1}`}
              onClick={() => setPaths((current) => current.filter((_, i) => i !== index))}
            >
              <Trash2 aria-hidden="true" />
            </Button>
          </div>
        ))}
        <div>
          <Button type="button" size="sm" onClick={() => setPaths((current) => [...current, ['', '']])}>
            <Plus aria-hidden="true" />
            Adicionar caminho
          </Button>
        </div>
      </fieldset>
      <SaveRow saving={saving} />
    </form>
  )
}

export function LibrarySettings() {
  return (
    <SectionCard
      name="biblioteca"
      title="Biblioteca"
      description="Onde os filmes moram, como cada lado enxerga o disco e a categoria dos downloads do acervo."
      status={(data: LibrarySection) => data.root_folders.length > 0}
      Form={LibraryForm}
    />
  )
}

// ---------------------------------------------------------------- limpeza

function CleanupForm({ data, save, saving }: FormProps<'limpeza'>) {
  const [policy, setPolicy] = useState<CleanupSection>(data)
  const [categories, setCategories] = useState(data.managed_categories.join('\n'))
  const [fraction, setFraction] = useState(String(Math.round(data.max_batch_fraction * 100)))
  const [ratio, setRatio] = useState(String(data.seed_ratio_alvo))
  const set = <K extends keyof CleanupSection>(key: K, value: CleanupSection[K]) =>
    setPolicy((current) => ({ ...current, [key]: value }))
  const number = (id: string, key: keyof CleanupSection, label: string, help?: string) => (
    <Field id={id} label={label} help={help}>
      <Input
        id={id}
        type="number"
        min={0}
        value={String(policy[key])}
        onChange={(event) => set(key, whole(event.target.value) as never)}
        aria-describedby={help ? `${id}-ajuda` : undefined}
        className="w-32 tabular-nums"
      />
    </Field>
  )
  return (
    <form
      noValidate
      className="mt-6 grid gap-5"
      onSubmit={(event) => {
        event.preventDefault()
        save({
          ...policy,
          managed_categories: lines(categories),
          max_batch_fraction: Math.min(100, whole(fraction)) / 100,
          seed_ratio_alvo: Math.max(0, Number(ratio.replace(',', '.')) || 0),
        })
      }}
    >
      <fieldset className="grid gap-4 sm:grid-cols-2">
        <legend className="mb-2 text-sm font-semibold">Órfãos de fila</legend>
        {number('limpeza-strikes', 'orphan_strikes', 'Strikes até sair', 'Ciclos seguidos como órfão antes de agir.')}
        <div className="grid gap-3 sm:col-span-2">
          <Toggle
            id="limpeza-privados"
            label="Apagar os arquivos de órfão em tracker privado"
            help="Desligado, o item sai da fila e o torrent fica."
            checked={policy.delete_private_orphans}
            onChange={(on) => set('delete_private_orphans', on)}
          />
          <Toggle
            id="limpeza-sem-cliente"
            label="Pular órfão cujo torrent já não está no cliente"
            checked={policy.skip_orphan_if_missing_in_client}
            onChange={(on) => set('skip_orphan_if_missing_in_client', on)}
          />
        </div>
      </fieldset>
      <fieldset className="grid gap-4 sm:grid-cols-2">
        <legend className="mb-2 text-sm font-semibold">Carências</legend>
        {number(
          'limpeza-seed',
          'private_seed_grace_hours',
          'Seed em privado sem vínculo (horas)',
          'Teto: sai de qualquer jeito depois disso. Zero apaga no ciclo seguinte — expõe a hit&run.',
        )}
        <Field
          id="limpeza-ratio"
          label="Ratio que libera o seed privado"
          help="Sai ao atingir este ratio, antes do teto. Zero desliga."
        >
          <Input
            id="limpeza-ratio"
            type="number"
            min={0}
            step={0.1}
            value={ratio}
            onChange={(event) => setRatio(event.target.value)}
            aria-describedby="limpeza-ratio-ajuda"
            className="w-32 tabular-nums"
          />
        </Field>
        {number(
          'limpeza-ocioso',
          'seed_ocioso_horas',
          'Seed privado sem envio há (horas)',
          'Sai se ninguém baixou nesse tempo, antes do teto. Zero desliga.',
        )}
        {number('limpeza-recente', 'recent_change_grace_hours', 'Arquivo mexido há menos de (horas)', 'Nunca é apagado.')}
      </fieldset>
      <fieldset className="grid gap-4 sm:grid-cols-2">
        <legend className="mb-2 text-sm font-semibold">Travas do lote</legend>
        {number('limpeza-gib', 'max_batch_gib', 'Máximo por ciclo (GiB)', 'Estourar aborta o ciclo inteiro.')}
        <Field id="limpeza-fracao" label="Máximo por ciclo (% da biblioteca)">
          <Input
            id="limpeza-fracao"
            type="number"
            min={0}
            max={100}
            value={fraction}
            onChange={(event) => setFraction(event.target.value)}
            className="w-32 tabular-nums"
          />
        </Field>
      </fieldset>
      <Field
        id="limpeza-categorias"
        label="Categorias gerenciadas"
        help="Uma por linha. Só seed nelas pode ser apagado por ter perdido o hardlink; vazia, essa regra não apaga nada."
      >
        <LinesInput
          id="limpeza-categorias"
          value={categories}
          onChange={setCategories}
          placeholder={'tv-sonarr\nacervo'}
          describedBy="limpeza-categorias-ajuda"
        />
      </Field>
      <SaveRow saving={saving} />
    </form>
  )
}

export function CleanupSettings() {
  return (
    <SectionCard
      name="limpeza"
      title="Limpeza"
      description="O ciclo tira da fila o que o gerenciador esqueceu e apaga o seed que perdeu o hardlink com a biblioteca. Não há simulação: as travas abortam o ciclo quando a leitura do mundo não é confiável."
      Form={CleanupForm}
    />
  )
}
