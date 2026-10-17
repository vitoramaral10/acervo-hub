import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Ban,
  Captions,
  CircleAlert,
  CircleCheck,
  CircleX,
  Download,
  FolderOpen,
  PackageOpen,
  Rocket,
  Send,
  Upload,
} from 'lucide-react'
import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { CopyButton } from '@/components/CopyButton'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Badge, Skeleton } from '@/components/ui/misc'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  type LaunchAdaptation,
  type LaunchCheck,
  type LaunchCheckStatus,
  type LaunchDetail,
  type LaunchFields,
  type LaunchItem,
  type LaunchSite,
  type LaunchState,
  type LaunchTrackerStatus,
  launchApi,
} from '@/lib/api'
import { formatAgo, formatCount, formatSize, safeHref } from '@/lib/format'
import { Chip, ChipGroup } from '@/pages/Movies'

const LIST_KEY = ['para-lancar'] as const
const PROGRESS_KEY = ['para-lancar', 'andamento'] as const
const detailKey = (hash: string, indexer: string) => ['para-lancar', hash, indexer] as const

/** O pacote em preparo muda de passo sozinho: a tela pergunta o andamento de novo a cada 3 s. */
const POLL_MS = 3000

type Filter = 'preparar' | 'prontos' | 'enviados' | 'inelegiveis' | 'todos'

const FILTERS: { id: Filter; label: string; states: LaunchState[] | null }[] = [
  { id: 'preparar', label: 'A preparar', states: ['a_preparar', 'preparando', 'falhou'] },
  { id: 'prontos', label: 'Prontos', states: ['pronto'] },
  { id: 'enviados', label: 'Enviados', states: ['enviado', 'semeando'] },
  { id: 'inelegiveis', label: 'Não elegíveis', states: ['inelegivel'] },
  // Descartados só aparecem aqui.
  { id: 'todos', label: 'Todos', states: null },
]

const SITES: { id: LaunchSite; label: string }[] = [
  { id: 'bj', label: 'BJ' },
  { id: 'amigos', label: 'Amigos' },
]

const STATES: Record<LaunchState, { label: string; tone: 'neutral' | 'accent' | 'success' | 'danger' | 'warning' }> = {
  a_preparar: { label: 'A preparar', tone: 'neutral' },
  preparando: { label: 'Preparando', tone: 'accent' },
  inelegivel: { label: 'Não elegível', tone: 'danger' },
  pronto: { label: 'Pronto', tone: 'success' },
  falhou: { label: 'Falhou', tone: 'danger' },
  enviado: { label: 'Enviado', tone: 'accent' },
  semeando: { label: 'Semeando', tone: 'success' },
  descartado: { label: 'Descartado', tone: 'neutral' },
}

const inFilter = (item: LaunchItem, filter: Filter) => {
  const states = FILTERS.find((f) => f.id === filter)?.states
  return states === null || states === undefined || states.includes(item.estado)
}

const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`

const itemTitle = (item: LaunchItem) => item.titulo ?? item.release ?? item.hash.slice(0, 8)

function StateBadge({ state }: { state: LaunchState }) {
  const { label, tone } = STATES[state]
  return <Badge tone={tone}>{label}</Badge>
}

function Problem({ title, message, onRetry }: { title: string; message: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
      <p className="font-medium">{title}</p>
      <p className="text-sm text-content-muted">{message}</p>
      <Button onClick={onRetry}>Tentar novamente</Button>
    </div>
  )
}

function ListSkeleton({ rows, label }: { rows: number; label: string }) {
  return (
    <div className="grid gap-2" aria-busy="true" aria-label={label}>
      {Array.from({ length: rows }, (_, key) => (
        <Skeleton key={key} className="h-16" />
      ))}
    </div>
  )
}

function Empty({ children }: { children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong p-10 text-center">
      <PackageOpen className="size-6 text-content-subtle" aria-hidden="true" />
      <p className="max-w-[55ch] text-sm text-content-muted">{children}</p>
    </div>
  )
}

/** Atualiza a lista e o detalhe do par depois de uma ação. */
function useRefreshLaunch() {
  const queryClient = useQueryClient()
  return (item: Pick<LaunchItem, 'hash' | 'indexer'>) => {
    void queryClient.invalidateQueries({ queryKey: LIST_KEY })
    void queryClient.invalidateQueries({ queryKey: detailKey(item.hash, item.indexer) })
  }
}

function usePrepare() {
  const refresh = useRefreshLaunch()
  return useMutation({
    mutationFn: (item: Pick<LaunchItem, 'hash' | 'indexer'>) => launchApi.prepare(item.hash, item.indexer),
    onSuccess: (_data, item) => {
      toast.success('Pacote na fila para preparo')
      refresh(item)
    },
    onError: (error: Error, item) => {
      toast.error(error.message)
      refresh(item)
    },
  })
}

function LaunchRow({ item, onOpen }: { item: LaunchItem; onOpen: () => void }) {
  const prepare = usePrepare()
  const busy = item.estado === 'preparando' || prepare.isPending
  // Já foi ao site: o publicado semeia destes arquivos (e da pasta adaptada), e o backend recusa preparar de novo.
  const sentToSite = item.estado === 'enviado' || item.estado === 'semeando' || Boolean(item.enviado_em)
  const locked = busy || sentToSite || !item.no_cliente
  const label = item.estado === 'a_preparar' || item.estado === 'descartado' ? 'Preparar' : 'Preparar de novo'
  const hasPackage = item.estado !== 'a_preparar'
  const title = itemTitle(item)
  return (
    <li className="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between">
      <div className="min-w-0">
        {hasPackage ? (
          <button
            type="button"
            onClick={onOpen}
            className="block max-w-full truncate rounded-sm text-left text-sm font-medium hover:text-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
            title={title}
            aria-label={`Abrir o pacote de ${title}`}
          >
            {title}
          </button>
        ) : (
          <p className="truncate text-sm font-medium" title={title}>
            {title}
          </p>
        )}
        {item.release && item.release !== title && (
          <p className="truncate font-mono text-xs text-content-muted" title={item.release}>
            {item.release}
          </p>
        )}
        <p className="mt-1 flex flex-wrap items-center gap-x-2 text-xs text-content-subtle tabular-nums">
          {item.tamanho !== null && <span>{formatSize(item.tamanho)}</span>}
          <span>
            {item.origem ?? 'fora do cliente'} → {item.destino ?? item.indexer}
          </span>
          {item.estado === 'preparando' && item.progresso && <span aria-live="polite">{item.progresso}</span>}
          {item.estado === 'falhou' && item.erro && <span className="text-danger">{item.erro}</span>}
        </p>
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-2">
        {item.parecido_no_destino && (
          <Badge tone="warning" title="O destino tem release de tamanho parecido, com arquivos diferentes">
            Parecido no destino
          </Badge>
        )}
        <StateBadge state={item.estado} />
        {hasPackage && (
          <Button variant="ghost" size="sm" onClick={onOpen} aria-label={`Ver o pacote de ${title}`}>
            Ver pacote
          </Button>
        )}
        <Button
          size="sm"
          variant={item.estado === 'a_preparar' ? 'primary' : 'secondary'}
          disabled={locked}
          loading={busy}
          title={
            sentToSite
              ? 'Já enviado ao site: o publicado semeia destes arquivos'
              : item.no_cliente
                ? undefined
                : 'O torrent de origem não está mais no cliente de download'
          }
          aria-label={`${label}: ${title}`}
          onClick={() => prepare.mutate(item)}
        >
          {label}
        </Button>
      </div>
    </li>
  )
}

const CHECK_GROUPS: { status: LaunchCheckStatus; title: string; icon: typeof CircleX; className: string }[] = [
  { status: 'fail', title: 'Reprovadas', icon: CircleX, className: 'text-danger' },
  { status: 'warn', title: 'Para você julgar', icon: CircleAlert, className: 'text-warning' },
  { status: 'ok', title: 'Aprovadas', icon: CircleCheck, className: 'text-success' },
]

function Checks({ checks }: { checks: LaunchCheck[] }) {
  return (
    <section aria-labelledby="lancar-checagens" className="grid gap-3">
      <h3 id="lancar-checagens" className="text-sm font-semibold">
        Checagem das regras
      </h3>
      {CHECK_GROUPS.map(({ status, title, icon: Icon, className }) => {
        const group = checks.filter((check) => check.status === status)
        if (group.length === 0) return null
        return (
          <div key={status}>
            <p className={`flex items-center gap-1.5 text-xs font-medium ${className}`}>
              <Icon className="size-4" aria-hidden="true" />
              {title} ({group.length})
            </p>
            <ul className="mt-1.5 grid gap-1.5 text-sm">
              {group.map((check, index) => (
                <li key={`${check.rule}-${index}`} className="flex gap-2 break-words">
                  <span className="shrink-0 font-mono text-xs text-content-subtle">{check.rule}</span>
                  <span>{check.message}</span>
                </li>
              ))}
            </ul>
          </div>
        )
      })}
    </section>
  )
}

const FIELD_LABELS: { key: keyof LaunchFields; label: string; long?: boolean }[] = [
  { key: 'titulo', label: 'Título' },
  { key: 'release', label: 'Release' },
  { key: 'ano', label: 'Ano' },
  { key: 'imdb', label: 'IMDb' },
  { key: 'tmdb', label: 'TMDB' },
  { key: 'duracao', label: 'Duração' },
  { key: 'tamanho', label: 'Tamanho' },
  { key: 'sinopse', label: 'Sinopse', long: true },
  { key: 'ficha', label: 'Ficha', long: true },
  { key: 'tecnico', label: 'Dados técnicos', long: true },
  { key: 'descricao', label: 'Descrição (BBCode)', long: true },
]

function FieldRow({ id, label, value, long }: { id: string; label: string; value: string; long?: boolean }) {
  return (
    <div className="grid gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <label htmlFor={id} className="text-sm font-medium">
          {label}
        </label>
        <CopyButton text={value} label={label.toLowerCase()} />
      </div>
      {long ? (
        <textarea
          id={id}
          readOnly
          value={value}
          rows={Math.min(12, Math.max(3, value.split('\n').length))}
          placeholder="Vazio: escreva à mão"
          className="w-full rounded-md border border-border-strong bg-surface px-3 py-2 font-mono text-xs text-content shadow-sm focus-visible:border-accent focus-visible:ring-3 focus-visible:ring-ring/25 focus-visible:outline-none"
        />
      ) : (
        <Input id={id} readOnly value={value} placeholder="Vazio" className="font-mono text-xs" />
      )}
    </div>
  )
}

/** O relatório do MediaInfo: o formulário do Amigos o pede, para colar ou para "Abrir arquivo .txt". */
function MediaInfoField({ detail, value }: { detail: LaunchDetail; value: string }) {
  return (
    <div className="grid gap-1.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span id="lancar-campo-mediainfo-rotulo" className="text-sm font-medium">
          MediaInfo
        </span>
        <div className="flex items-center gap-1">
          {value && (
            <Button variant="ghost" size="sm" asChild>
              <a href={launchApi.mediainfoUrl(detail.hash, detail.indexer)} download>
                <Download aria-hidden="true" />
                Baixar .txt
              </a>
            </Button>
          )}
          <CopyButton text={value} label="mediainfo" />
        </div>
      </div>
      <pre
        id="lancar-campo-mediainfo"
        aria-labelledby="lancar-campo-mediainfo-rotulo"
        tabIndex={0}
        className="max-h-80 overflow-auto rounded-md border border-border-strong bg-surface px-3 py-2 font-mono text-xs whitespace-pre text-content shadow-sm focus-visible:border-accent focus-visible:ring-3 focus-visible:ring-ring/25 focus-visible:outline-none"
      >
        {value || 'Vazio: o MediaInfo não gerou o relatório'}
      </pre>
    </div>
  )
}

/** O nome do arquivo, sem as pastas. */
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1)

function adaptationSource(adaptation: LaunchAdaptation) {
  const track = adaptation.faixa ? ` (${adaptation.faixa})` : ''
  return `faixa ${adaptation.stream}${track}${adaptation.forced ? ', forçada' : ''}`
}

/**
 * A pasta adaptada do BJ: o que o laudo manda extrair e, montada, onde ela está e o que tem. O vídeo não muda; a pasta
 * nova tem hardlinks da origem e o `.srt` ao lado.
 */
function AdaptedFolder({ detail }: { detail: LaunchDetail }) {
  const adaptations = detail.report.adaptations ?? []
  const folder = detail.pasta_adaptada
  const layout = detail.layout_adaptado ?? []
  const built = folder !== null && ['pronto', 'enviado', 'semeando'].includes(detail.estado)
  if (adaptations.length === 0 && !built) return null
  return (
    <section aria-labelledby="lancar-pasta" className="grid gap-3">
      <h3 id="lancar-pasta" className="flex items-center gap-1.5 text-sm font-semibold">
        <Captions className="size-4" aria-hidden="true" />
        Legenda em arquivo separado
      </h3>
      <p className="text-sm text-content-muted">
        O {detail.destino} exige a legenda PT ao lado do vídeo. O vídeo não muda: o acervo monta uma pasta nova com
        hardlinks dos arquivos da origem e a legenda extraída, e o .torrent é dessa pasta.
      </p>
      {adaptations.length > 0 && (
        <ul className="grid gap-1.5 text-sm">
          {adaptations.map((adaptation) => (
            <li key={adaptation.output} className="flex flex-wrap gap-x-1.5 break-words">
              <span className="text-content-muted">{built ? 'criou:' : 'vai criar:'}</span>
              <span className="font-mono text-xs leading-5">{baseName(adaptation.output)}</span>
              <span className="text-content-muted">a partir da {adaptationSource(adaptation)}</span>
            </li>
          ))}
        </ul>
      )}
      {built && folder && (
        <div className="grid gap-2 rounded-md border border-border bg-surface-raised p-3">
          <div className="flex items-center justify-between gap-2">
            <p className="flex min-w-0 items-center gap-1.5 text-sm font-medium">
              <FolderOpen className="size-4 shrink-0" aria-hidden="true" />
              <span>Pasta adaptada</span>
            </p>
            <CopyButton text={folder} label="caminho da pasta adaptada" />
          </div>
          <p className="font-mono text-xs break-all">{folder}</p>
          {layout.length > 0 && (
            <ul className="grid gap-1 text-xs" aria-label="Arquivos da pasta adaptada">
              {layout.map(([path, size]) => (
                <li key={path} className="flex flex-wrap justify-between gap-x-3">
                  <span className="min-w-0 break-all font-mono">{path}</span>
                  <span className="shrink-0 text-content-muted tabular-nums">{formatSize(size)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </section>
  )
}

const TRACKER_TONES: Record<LaunchTrackerStatus, 'success' | 'warning' | 'danger' | 'neutral'> = {
  funcionando: 'success',
  'não registrado': 'warning',
  erro: 'danger',
  'sem resposta': 'neutral',
}

/**
 * O pacote enviado semeia parado-e-conferido antes da aprovação (os sites só liberam com seeder). Mostra o status do
 * tracker do destino e deixa mandar o `.torrent` que o site entregou, para quando ele troca o hash.
 */
function SeedSection({ detail }: { detail: LaunchDetail }) {
  const refresh = useRefreshLaunch()
  const input = useRef<HTMLInputElement>(null)
  const upload = useMutation({
    mutationFn: (file: File) => launchApi.siteTorrent(detail.hash, detail.indexer, file),
    onSuccess: (data) => {
      if (data.aviso) {
        toast.warning(data.aviso)
      } else {
        toast.success(
          data.adicionado ? 'Torrent do site no cliente: confere os arquivos e semeia com 100%' : 'Esse torrent já está no cliente',
        )
      }
      refresh(detail)
    },
    onError: (error: Error) => toast.error(error.message),
  })
  const status = detail.status_tracker
  return (
    <section aria-labelledby="lancar-semear" className="grid gap-3">
      <h3 id="lancar-semear" className="text-sm font-semibold">
        Semear antes da aprovação
      </h3>
      <p className="text-sm text-content-muted">
        O site só libera o envio com seeder. O acervo adiciona o .torrent do pacote ao cliente, parado, confere os
        arquivos e só inicia com 100%.
      </p>
      {status && (
        <p className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-content-muted">Tracker do {detail.destino}:</span>
          <Badge tone={TRACKER_TONES[status]}>{status}</Badge>
          {detail.torrent_do_site && <span className="text-xs text-content-subtle">(torrent do site)</span>}
        </p>
      )}
      {status === 'não registrado' && (
        <p role="status" className="text-sm text-content-muted">
          Logo depois do envio isso é esperado: o site ainda processa ou espera aprovar. Se continuar assim, baixe o
          .torrent no site e envie aqui, porque o hash pode ter mudado.
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <input
          ref={input}
          type="file"
          accept=".torrent,application/x-bittorrent"
          className="sr-only"
          aria-label="Arquivo .torrent do site"
          tabIndex={-1}
          onChange={(event) => {
            const file = event.target.files?.[0]
            event.target.value = ''
            if (file) upload.mutate(file)
          }}
        />
        <Button variant="secondary" loading={upload.isPending} onClick={() => input.current?.click()}>
          <Upload aria-hidden="true" />
          Enviar o .torrent do site
        </Button>
      </div>
    </section>
  )
}

/** Os quadros do pacote na resolução real: o formulário do site pede o arquivo, não o link do ImgBB. */
function Captures({ detail }: { detail: LaunchDetail }) {
  if (detail.estado !== 'pronto' && detail.estado !== 'enviado' && detail.estado !== 'semeando') return null
  const video = detail.report.capturas
  return (
    <section aria-labelledby="lancar-capturas" className="grid gap-3">
      <h3 id="lancar-capturas" className="text-sm font-semibold">
        Capturas
      </h3>
      <p className="text-xs text-content-muted">
        Tiradas na hora, do vídeo da origem, sem reduzir. Anexe estas imagens no formulário do site (PNG; JPEG na mesma resolução quando o PNG passa de 5 MB, o limite do Amigos).
      </p>
      <ul className="grid gap-2 sm:grid-cols-2">
        {[1, 2, 3, 4].map((number) => (
          <li key={number} className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <Button variant="secondary" size="sm" asChild>
              <a href={launchApi.captureUrl(detail.hash, detail.indexer, number)} download>
                <Download aria-hidden="true" />
                Baixar captura {number}
              </a>
            </Button>
            {video && (
              <span className="text-xs text-content-muted tabular-nums">
                {video.largura}×{video.altura} — resolução original
              </span>
            )}
          </li>
        ))}
      </ul>
      {detail.screens.length > 0 && (
        <div className="grid gap-1.5">
          <h4 className="text-xs font-medium">Links diretos (resolução original)</h4>
          <ul className="grid gap-1">
            {detail.screens.map((screen, index) => (
              <li key={screen.url} className="flex items-center justify-between gap-2 text-xs">
                <a
                  href={safeHref(screen.url)}
                  target="_blank"
                  rel="noreferrer"
                  className="min-w-0 break-all font-mono hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                  aria-label={`Abrir o link direto da captura ${index + 1}, em nova aba`}
                >
                  {screen.url}
                </a>
                <CopyButton text={screen.url} label={`link da captura ${index + 1}`} />
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  )
}

function Package({ detail, item }: { detail: LaunchDetail; item: LaunchItem }) {
  const refresh = useRefreshLaunch()
  const prepare = usePrepare()
  const sent = useMutation({
    mutationFn: () => launchApi.sent(detail.hash, detail.indexer),
    onSuccess: (data) => {
      if (data.aviso) toast.warning(data.aviso)
      else toast.success('Marcado como enviado: o .torrent entrou no cliente, parado, e o acervo semeia sozinho')
      refresh(detail)
    },
    onError: (error: Error) => toast.error(error.message),
  })
  const discard = useMutation({
    mutationFn: () => launchApi.discard(detail.hash, detail.indexer),
    onSuccess: (data) => {
      if (data.aviso) toast.warning(`Pacote descartado, mas ${data.aviso}`)
      else toast.success('Pacote descartado')
      refresh(detail)
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const checks = detail.report.checks ?? []
  const destination = detail.report.destination ?? []
  const fields = detail.fields
  const preparing = detail.estado === 'preparando'
  const done = detail.estado === 'enviado' || detail.estado === 'semeando'
  // Já foi ao site: o publicado segue valendo, então o backend recusa descartar.
  const sentToSite = Boolean(detail.enviado_em) && detail.estado !== 'semeando'

  return (
    <div className="grid gap-6">
      <div className="flex flex-wrap items-center gap-2">
        <StateBadge state={detail.estado} />
        {detail.enviado_em && <span className="text-xs text-content-muted">enviado {formatAgo(detail.enviado_em)}</span>}
        {detail.atualizado_em && (
          <span className="text-xs text-content-subtle">atualizado {formatAgo(detail.atualizado_em)}</span>
        )}
      </div>

      {preparing && (
        <p role="status" aria-live="polite" className="rounded-md border border-border bg-surface-raised p-3 text-sm">
          {detail.progresso ?? 'Preparando…'}
          <span className="block text-xs text-content-muted">Atualiza sozinho a cada 3 segundos.</span>
        </p>
      )}
      {detail.estado === 'falhou' && (
        <p role="alert" className="rounded-md border border-border bg-danger-bg p-3 text-sm text-danger break-words">
          {detail.erro ?? 'Falha ao preparar o pacote.'}
        </p>
      )}

      {detail.estado === 'enviado' && detail.erro && (
        <p role="alert" className="rounded-md border border-border bg-warning-bg p-3 text-sm text-warning break-words">
          {detail.erro}
        </p>
      )}

      {(detail.estado === 'enviado' || detail.estado === 'semeando') && <SeedSection detail={detail} />}

      {checks.length > 0 && <Checks checks={checks} />}

      <AdaptedFolder detail={detail} />

      {destination.length > 0 && (
        <section aria-labelledby="lancar-destino" className="grid gap-2">
          <h3 id="lancar-destino" className="text-sm font-semibold">
            Já existe no {detail.destino}
          </h3>
          <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border">
            {destination.map((release) => (
              <li key={release.title} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 text-xs">
                <span className="min-w-0 break-all font-mono">{release.title}</span>
                <span className="shrink-0 text-content-muted tabular-nums">
                  {release.quality} · {release.audio}
                  {release.codec ? ` · ${release.codec.toUpperCase()}` : ''} · {formatSize(release.size)}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {fields.titulo !== undefined && (
        <section aria-labelledby="lancar-campos" className="grid gap-4">
          <h3 id="lancar-campos" className="text-sm font-semibold">
            Campos do envio
          </h3>
          {FIELD_LABELS.map(({ key, label, long }) => (
            <FieldRow key={key} id={`lancar-campo-${key}`} label={label} value={fields[key] ?? ''} long={long} />
          ))}
          <MediaInfoField detail={detail} value={fields.mediainfo ?? ''} />
        </section>
      )}

      <Captures detail={detail} />

      <div className="flex flex-wrap gap-2 border-t border-border pt-4">
        {detail.tem_torrent && (
          <Button variant="primary" asChild>
            <a href={launchApi.torrentUrl(detail.hash, detail.indexer)} download>
              <Download aria-hidden="true" />
              Baixar .torrent
            </a>
          </Button>
        )}
        {detail.estado === 'pronto' && (
          <Button variant="secondary" loading={sent.isPending} onClick={() => sent.mutate()}>
            <Send aria-hidden="true" />
            Marquei como enviado
          </Button>
        )}
        {!preparing && !done && !detail.enviado_em && (
          <Button
            variant="ghost"
            loading={prepare.isPending}
            disabled={!item.no_cliente}
            onClick={() => prepare.mutate(detail)}
          >
            Preparar de novo
          </Button>
        )}
        {!preparing && detail.estado !== 'descartado' && !sentToSite && (
          <Button variant="ghost" loading={discard.isPending} onClick={() => discard.mutate()}>
            <Ban aria-hidden="true" />
            Descartar
          </Button>
        )}
      </div>
    </div>
  )
}

function PackageDialog({ item, onClose }: { item: LaunchItem; onClose: () => void }) {
  const detail = useQuery({
    queryKey: detailKey(item.hash, item.indexer),
    queryFn: () => launchApi.detail(item.hash, item.indexer),
    refetchInterval: (query) => (query.state.data?.estado === 'preparando' ? POLL_MS : false),
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>{itemTitle(item)}</DialogTitle>
          <DialogDescription>
            Pacote para o {item.destino ?? item.indexer}. Confira, copie os campos e envie pelo site: o acervo não envia
            nada sozinho.
          </DialogDescription>
        </DialogHeader>
        {detail.isPending ? (
          <ListSkeleton rows={4} label="Carregando o pacote" />
        ) : detail.isError ? (
          <Problem title="Não deu para abrir o pacote" message={detail.error.message} onRetry={() => void detail.refetch()} />
        ) : (
          <Package detail={detail.data} item={item} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function SitePanel({
  site,
  items,
  filter,
  onOpen,
}: {
  site: LaunchSite
  items: LaunchItem[]
  filter: Filter
  onOpen: (item: LaunchItem) => void
}) {
  const own = items.filter((item) => item.site === site)
  const shown = own.filter((item) => inFilter(item, filter))
  const name = SITES.find((s) => s.id === site)?.label ?? site
  if (shown.length === 0) {
    return (
      <Empty>
        {own.length === 0
          ? `Nada para lançar no ${name} por enquanto. Os candidatos aparecem quando o cross-seed confirma que um release do acervo não existe lá.`
          : `Nenhum pacote do ${name} neste filtro.`}
      </Empty>
    )
  }
  return (
    <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-surface">
      {shown.map((item) => (
        <LaunchRow key={`${item.hash}/${item.indexer}`} item={item} onOpen={() => onOpen(item)} />
      ))}
    </ul>
  )
}

export function ToLaunchPage() {
  const [site, setSite] = useState<LaunchSite>('bj')
  const [filter, setFilter] = useState<Filter>('preparar')
  const [open, setOpen] = useState<LaunchItem | null>(null)

  const queryClient = useQueryClient()
  const list = useQuery({ queryKey: LIST_KEY, queryFn: launchApi.list })
  const listed = useMemo(() => list.data?.itens ?? [], [list.data])
  const waiting = useMemo(() => listed.filter((item) => item.estado === 'preparando'), [listed])
  // A lista loga no qBittorrent e recarrega o catálogo inteiro: não se repete a
  // cada 3 s. Enquanto há pacote em preparo, só o andamento (uma consulta ao
  // banco) é repetido, e a lista é refeita quando algum deixa de preparar.
  const progress = useQuery({
    queryKey: PROGRESS_KEY,
    queryFn: launchApi.progress,
    enabled: waiting.length > 0,
    refetchInterval: POLL_MS,
  })
  const preparing = progress.data?.itens
  const settled = !progress.isFetching && waiting.length > 0
  useEffect(() => {
    if (!settled || !preparing) return
    const still = new Set(preparing.map((p) => `${p.hash}/${p.indexer}`))
    if (waiting.some((item) => !still.has(`${item.hash}/${item.indexer}`))) {
      void queryClient.invalidateQueries({ queryKey: LIST_KEY })
    }
  }, [settled, preparing, waiting, queryClient])
  const items = useMemo(() => {
    const steps = new Map(preparing?.map((p) => [`${p.hash}/${p.indexer}`, p.progresso]))
    return listed.map((item) =>
      item.estado === 'preparando' && steps.has(`${item.hash}/${item.indexer}`)
        ? { ...item, progresso: steps.get(`${item.hash}/${item.indexer}`) ?? null }
        : item,
    )
  }, [listed, preparing])
  const count = (id: LaunchSite) => items.filter((item) => item.site === id && item.estado !== 'descartado').length
  const ofSite = items.filter((item) => item.site === site)

  return (
    <>
      <PageHeader
        title="Para lançar"
        description="Releases do acervo que existem num tracker e não no outro. O acervo prepara o pacote (regras, título, descrição, capturas e .torrent); você revisa e envia pelo site."
      />

      {list.isPending ? (
        <ListSkeleton rows={4} label="Carregando os candidatos" />
      ) : list.isError ? (
        <Problem title="Não deu para listar os candidatos" message={list.error.message} onRetry={() => void list.refetch()} />
      ) : (
        <>
          {list.data.aviso && (
            <p role="status" className="mb-4 rounded-md border border-border bg-warning-bg p-3 text-sm text-warning">
              {list.data.aviso}
            </p>
          )}
          <Tabs value={site} onValueChange={(value) => setSite(value as LaunchSite)}>
            <TabsList aria-label="Destino do lançamento">
              {SITES.map((s) => (
                <TabsTrigger key={s.id} value={s.id}>
                  <Rocket aria-hidden="true" />
                  {s.label}
                  <span className="text-xs text-content-subtle tabular-nums">{count(s.id)}</span>
                </TabsTrigger>
              ))}
            </TabsList>
            <div className="mt-4 mb-4">
              <ChipGroup label="Filtrar por estado" single>
                {FILTERS.map((f) => {
                  const n = ofSite.filter((item) => inFilter(item, f.id)).length
                  return (
                    <Chip key={f.id} single active={filter === f.id} onClick={() => setFilter(f.id)}>
                      {f.label} ({n})
                    </Chip>
                  )
                })}
              </ChipGroup>
              <p className="sr-only" role="status">
                {plural(ofSite.filter((item) => inFilter(item, filter)).length, 'pacote', 'pacotes')} neste filtro
              </p>
            </div>
            {SITES.map((s) => (
              <TabsContent key={s.id} value={s.id}>
                <SitePanel site={s.id} items={items} filter={filter} onOpen={setOpen} />
              </TabsContent>
            ))}
          </Tabs>
        </>
      )}

      {open && <PackageDialog item={open} onClose={() => setOpen(null)} />}
    </>
  )
}
