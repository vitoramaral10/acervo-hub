// As seções da configuração do serviço, cada uma gravada no banco por
// `/ui/api/configuracoes/{secao}`. Segredo nunca volta da API: o campo vem
// vazio, com "Definida" no placeholder, e salvar em branco mantém o guardado.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, CircleDashed, KeyRound, RefreshCw } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge, Skeleton } from '@/components/ui/misc'
import {
  type DownloadClientSection,
  type JellyfinSection,
  type LibrarySection,
  type SectionInput,
  type SectionName,
  type Sections,
  type ServerSection,
  api,
} from '@/lib/api'
import { Field } from '@/pages/settings/Rules'

const sectionKey = (name: SectionName) => ['configuracao', name] as const

/** Lê e grava uma seção; a resposta do PUT já é a seção como ficou. */
function useSection<N extends SectionName>(name: N) {
  const queryClient = useQueryClient()
  const query = useQuery({ queryKey: sectionKey(name), queryFn: () => api.section(name) })
  const save = useMutation({
    mutationFn: (value: SectionInput<N>) => api.saveSection(name, value),
    onSuccess: (data) => {
      queryClient.setQueryData(sectionKey(name), data)
      // As tarefas (disponíveis ou não) dependem da configuração.
      void queryClient.invalidateQueries({ queryKey: ['tarefas'] })
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
  const [catalogs, setCatalogs] = useState(data.catalogos.join('\n'))
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
          catalogos: lines(catalogs),
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
        help="Abre a interface no cabeçalho X-Api-Key, para script e automação. Ao menos 16 caracteres. Trocar derruba quem usa a antiga."
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
      <SaveRow saving={saving} />
    </form>
  )
}

export function ServerSettings() {
  return (
    <SectionCard
      name="servidor"
      title="Servidor"
      description="A chave de API da interface, a rede dos indexadores e o catálogo de definições."
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
  return (
    <form
      noValidate
      className="mt-6 grid gap-4"
      onSubmit={(event) => {
        event.preventDefault()
        const value: SectionInput<'jellyfin'> = { url: url.trim() }
        if (key.trim()) value.api_key = key.trim()
        save(value)
      }}
    >
      <Field id="jellyfin-url" label="URL" help="Em branco, a tela “Para apagar” não sugere assistidos.">
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
        help="Painel → Chaves de API. A varredura da biblioteca, pedida depois de importar ou apagar, exige chave de administrador."
      >
        <SecretInput
          id="jellyfin-chave"
          defined={data.api_key.definida}
          value={key}
          onChange={setKey}
          describedBy="jellyfin-chave-ajuda"
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
      description="Diz o que já foi assistido. Com ele, a tela “Para apagar” sugere o filme ou a temporada vista há mais que a carência; nada sai sem você marcar e confirmar."
      status={(data: JellyfinSection) => data.url !== ''}
      Form={JellyfinForm}
    />
  )
}

// ---------------------------------------------------------------- biblioteca

function LibraryForm({ data, save, saving }: FormProps<'biblioteca'>) {
  const [roots, setRoots] = useState(data.roots.join('\n'))
  const [folders, setFolders] = useState(data.root_folders.join('\n'))
  const [category, setCategory] = useState(data.category)
  const [seriesRoot, setSeriesRoot] = useState(data.series_root)
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
          series_root: seriesRoot.trim(),
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
      <Field id="biblioteca-series" label="Pasta raiz das séries">
        <Input id="biblioteca-series" value={seriesRoot} onChange={(event) => setSeriesRoot(event.target.value)} />
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
        help="Categoria usada pelo acervo e gerenciada pela limpeza."
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
      <SaveRow saving={saving} />
    </form>
  )
}

export function LibrarySettings() {
  return (
    <SectionCard
      name="biblioteca"
      title="Biblioteca"
      description="Pastas dos filmes e séries e a categoria dos downloads do acervo. Os caminhos são os mesmos para o serviço e o cliente."
      status={(data: LibrarySection) => data.root_folders.length > 0}
      Form={LibraryForm}
    />
  )
}
