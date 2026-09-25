import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Ban, ChevronLeft, ChevronRight, CircleAlert, Download, History, Inbox, ShieldOff, X } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { EVENTS, HistoryList } from '@/components/HistoryList'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { type HistoryEventKind, type QueueItem, library } from '@/lib/api'
import { formatAgo, formatCount, formatSize } from '@/lib/format'
import { cn } from '@/lib/utils'

function formatEta(seconds: number): string {
  if (seconds < 60) return 'menos de 1 min'
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes} min`
  const hours = Math.floor(minutes / 60)
  return `${hours} h ${minutes % 60} min`
}

function ErrorBox({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
      <p className="font-medium">Não foi possível carregar.</p>
      <p className="text-sm text-content-muted">{message}</p>
      <Button onClick={onRetry}>Tentar novamente</Button>
    </div>
  )
}

function Empty({ icon: Icon, title, text }: { icon: typeof Inbox; title: string; text: string }) {
  return (
    <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong px-6 py-14 text-center">
      <Icon className="size-7 text-content-subtle" aria-hidden="true" />
      <p className="font-medium">{title}</p>
      <p className="max-w-md text-sm text-content-muted">{text}</p>
    </div>
  )
}

function RemoveDialog({ item, onClose }: { item: QueueItem | null; onClose: () => void }) {
  const queryClient = useQueryClient()
  const [fromClient, setFromClient] = useState(true)
  const [block, setBlock] = useState(false)
  const [search, setSearch] = useState(false)
  const remove = useMutation({
    mutationFn: () =>
      library.removeDownload(item!.id, { remover_do_cliente: fromClient, bloquear: block, buscar: search }),
    onSuccess: () => {
      toast.success(block ? 'Marcado como falho e bloqueado' : 'Tirado da fila')
      onClose()
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ['fila'] })
      void queryClient.invalidateQueries({ queryKey: ['historico'] })
      void queryClient.invalidateQueries({ queryKey: ['bloqueados'] })
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
    },
  })
  return (
    <Dialog open={item !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Tirar da fila</DialogTitle>
          <DialogDescription className="font-mono text-xs break-all">{item?.release}</DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          {[
            {
              id: 'cliente',
              label: 'Apagar do qBittorrent',
              help: 'Remove o torrent e o que já baixou.',
              value: fromClient,
              set: setFromClient,
            },
            {
              id: 'bloquear',
              label: 'Bloquear este release',
              help: 'Ele não é pego de novo — use quando o release é ruim.',
              value: block,
              set: setBlock,
            },
            {
              id: 'buscar',
              label: 'Buscar outro agora',
              help: 'Procura e pega o melhor que sobrar.',
              value: search,
              set: setSearch,
            },
          ].map((option) => (
            <div key={option.id} className="flex items-start justify-between gap-4">
              <div>
                <Label htmlFor={`remover-${option.id}`}>{option.label}</Label>
                <p id={`remover-${option.id}-ajuda`} className="text-xs text-content-subtle">
                  {option.help}
                </p>
              </div>
              <Switch
                id={`remover-${option.id}`}
                checked={option.value}
                onCheckedChange={option.set}
                aria-describedby={`remover-${option.id}-ajuda`}
              />
            </div>
          ))}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancelar
          </Button>
          <Button variant="primary" loading={remove.isPending} onClick={() => remove.mutate()}>
            Tirar da fila
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function QueueRow({ item, onRemove }: { item: QueueItem; onRemove: () => void }) {
  const progress = item.progresso ?? 0
  const warning = item.mensagem?.startsWith('importação')
  const done = progress >= 1
  return (
    <li className="flex gap-4 py-4">
      <div className="aspect-[2/3] w-12 shrink-0 overflow-hidden rounded-sm border border-border bg-surface-raised">
        {item.poster && <img src={item.poster} alt="" loading="lazy" className="size-full object-cover" />}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <p className="font-medium">{item.filme ?? `Filme ${item.filme_id}`}</p>
          <Badge>{item.qualidade}</Badge>
          {item.upgrade && <Badge tone="accent">Upgrade</Badge>}
        </div>
        <p className="mt-0.5 truncate font-mono text-xs text-content-subtle" title={item.release}>
          {item.release}
        </p>
        <div className="mt-2 flex items-center gap-3">
          <div
            className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-raised"
            role="progressbar"
            aria-label="Progresso do download"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(progress * 100)}
          >
            <div
              className={cn('h-full rounded-full transition-[width]', done ? 'bg-success' : 'bg-accent')}
              style={{ width: `${Math.max(progress * 100, item.no_cliente ? 1 : 0)}%` }}
            />
          </div>
          <span className="w-10 text-right text-xs text-content-muted tabular-nums">{Math.floor(progress * 100)}%</span>
        </div>
        <p className="mt-1.5 flex flex-wrap gap-x-3 text-xs text-content-subtle tabular-nums">
          <span>{formatSize(item.tamanho)}</span>
          {!done && item.velocidade ? <span>{formatSize(item.velocidade)}/s</span> : null}
          {!done && item.restante_segundos ? <span>faltam {formatEta(item.restante_segundos)}</span> : null}
          {item.seeds != null && !done ? <span>{formatCount(item.seeds)} seeds</span> : null}
          <span>{item.indexador}</span>
          <span>pego {formatAgo(item.pego_em)}</span>
          {done && !warning && <span className="text-success">baixado — importa na próxima rodada</span>}
        </p>
        {!item.no_cliente && (
          <p className="mt-1.5 flex items-center gap-1.5 text-xs text-warning">
            <CircleAlert className="size-3.5" aria-hidden="true" />O qBittorrent não respondeu ou não tem mais este
            torrent.
          </p>
        )}
        {warning && (
          <p className="mt-1.5 flex items-center gap-1.5 text-xs text-warning">
            <CircleAlert className="size-3.5 shrink-0" aria-hidden="true" />
            {item.mensagem}
          </p>
        )}
      </div>
      <Button variant="ghost" size="sm" aria-label={`Tirar ${item.filme ?? 'download'} da fila`} onClick={onRemove}>
        <X aria-hidden="true" />
      </Button>
    </li>
  )
}

function QueueTab() {
  const queue = useQuery({ queryKey: ['fila'], queryFn: library.queue, refetchInterval: 5_000 })
  const [removing, setRemoving] = useState<QueueItem | null>(null)
  if (queue.isPending) return <ListSkeleton rows={3} />
  if (queue.isError) return <ErrorBox message={queue.error.message} onRetry={() => void queue.refetch()} />
  if (queue.data.fila.length === 0) {
    return (
      <Empty
        icon={Download}
        title="Nada baixando"
        text="O que o acervo-hub pegar aparece aqui, com o progresso do qBittorrent, até ser importado."
      />
    )
  }
  return (
    <>
      <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-4">
        {queue.data.fila.map((item) => (
          <QueueRow key={item.id} item={item} onRemove={() => setRemoving(item)} />
        ))}
      </ul>
      <RemoveDialog item={removing} onClose={() => setRemoving(null)} />
    </>
  )
}

const PAGE = 50
const EVENT_FILTERS: (HistoryEventKind | 'todos')[] = [
  'todos',
  'grabbed',
  'imported',
  'upgraded',
  'failed',
  'movie_added',
  'movie_deleted',
  'file_deleted',
]

function HistoryTab() {
  const [page, setPage] = useState(1)
  const [event, setEvent] = useState<HistoryEventKind | 'todos'>('todos')
  const history = useQuery({
    queryKey: ['historico', page, event],
    queryFn: () => library.history({ pagina: page, tamanho: PAGE, evento: event === 'todos' ? undefined : event }),
    placeholderData: keepPreviousData,
  })
  const pages = history.data ? Math.max(1, Math.ceil(history.data.total / PAGE)) : 1
  return (
    <div>
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="w-56">
          <Label htmlFor="historico-evento" className="sr-only">
            Tipo de evento
          </Label>
          <Select
            value={event}
            onValueChange={(value) => {
              setEvent(value as HistoryEventKind | 'todos')
              setPage(1)
            }}
          >
            <SelectTrigger id="historico-evento">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {EVENT_FILTERS.map((value) => (
                <SelectItem key={value} value={value}>
                  {value === 'todos' ? 'Todos os eventos' : EVENTS[value].label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {history.data && (
          <p className="text-xs text-content-subtle tabular-nums">{formatCount(history.data.total)} eventos</p>
        )}
      </div>
      {history.isPending ? (
        <ListSkeleton rows={6} />
      ) : history.isError ? (
        <ErrorBox message={history.error.message} onRetry={() => void history.refetch()} />
      ) : history.data.eventos.length === 0 ? (
        <Empty
          icon={History}
          title={event === 'todos' ? 'Histórico vazio' : 'Nenhum evento desse tipo'}
          text={
            event === 'todos'
              ? 'Cada grab, importação, falha e remoção fica registrada aqui. Para trazer o histórico do Radarr, use Configurações → Migração.'
              : 'Troque o filtro para ver os outros eventos.'
          }
        />
      ) : (
        <div
          className={cn('rounded-lg border border-border bg-surface px-4', history.isPlaceholderData && 'opacity-60')}
        >
          <HistoryList events={history.data.eventos} showMovie />
        </div>
      )}
      {pages > 1 && (
        <nav aria-label="Páginas do histórico" className="mt-4 flex items-center justify-end gap-2">
          <Button
            variant="ghost"
            size="sm"
            disabled={page <= 1}
            onClick={() => setPage((p) => p - 1)}
            aria-label="Página anterior"
          >
            <ChevronLeft aria-hidden="true" />
          </Button>
          <span className="text-xs text-content-muted tabular-nums">
            {page} de {pages}
          </span>
          <Button
            variant="ghost"
            size="sm"
            disabled={page >= pages}
            onClick={() => setPage((p) => p + 1)}
            aria-label="Próxima página"
          >
            <ChevronRight aria-hidden="true" />
          </Button>
        </nav>
      )}
    </div>
  )
}

function BlocklistTab() {
  const queryClient = useQueryClient()
  const blocked = useQuery({ queryKey: ['bloqueados'], queryFn: library.blocklist })
  const unblock = useMutation({
    mutationFn: library.unblock,
    onSuccess: () => toast.success('Release desbloqueado'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['bloqueados'] }),
  })
  if (blocked.isPending) return <ListSkeleton rows={3} />
  if (blocked.isError) return <ErrorBox message={blocked.error.message} onRetry={() => void blocked.refetch()} />
  if (blocked.data.bloqueados.length === 0) {
    return (
      <Empty
        icon={ShieldOff}
        title="Nenhum release bloqueado"
        text="Download que falha, ou que você marca como falho, entra aqui e não é pego de novo."
      />
    )
  }
  return (
    <ul className="divide-y divide-border rounded-lg border border-border bg-surface px-4">
      {blocked.data.bloqueados.map((item) => (
        <li key={item.id} className="flex items-start gap-3 py-3">
          <Ban className="mt-0.5 size-4 shrink-0 text-danger" aria-hidden="true" />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{item.filme ?? 'Filme fora do catálogo'}</p>
            <p className="truncate font-mono text-xs text-content-subtle" title={item.source_title}>
              {item.source_title}
            </p>
            <p className="mt-0.5 text-xs text-content-subtle">
              {[item.message, item.quality, item.indexer, formatAgo(item.at)].filter(Boolean).join(' · ')}
            </p>
          </div>
          <Button
            variant="ghost"
            size="sm"
            loading={unblock.isPending && unblock.variables === item.id}
            onClick={() => unblock.mutate(item.id)}
          >
            Desbloquear
          </Button>
        </li>
      ))}
    </ul>
  )
}

function ListSkeleton({ rows }: { rows: number }) {
  return (
    <div aria-busy="true" aria-label="Carregando" className="grid gap-3">
      {Array.from({ length: rows }, (_, key) => (
        <Skeleton key={key} className="h-16 rounded-lg" />
      ))}
    </div>
  )
}

export function ActivityPage() {
  const queue = useQuery({ queryKey: ['fila'], queryFn: library.queue, refetchInterval: 5_000 })
  const count = queue.data?.fila.length ?? 0
  return (
    <>
      <PageHeader
        title="Atividade"
        description="O que está baixando, o que já aconteceu com cada filme e os releases que não devem ser pegos de novo."
      />
      <Tabs defaultValue="fila">
        <TabsList>
          <TabsTrigger value="fila">
            <Download aria-hidden="true" />
            Fila
            {count > 0 && <Badge tone="accent">{count}</Badge>}
          </TabsTrigger>
          <TabsTrigger value="historico">
            <History aria-hidden="true" />
            Histórico
          </TabsTrigger>
          <TabsTrigger value="bloqueados">
            <Ban aria-hidden="true" />
            Bloqueados
          </TabsTrigger>
        </TabsList>
        <TabsContent value="fila">
          <QueueTab />
        </TabsContent>
        <TabsContent value="historico">
          <HistoryTab />
        </TabsContent>
        <TabsContent value="bloqueados">
          <BlocklistTab />
        </TabsContent>
      </Tabs>
    </>
  )
}
