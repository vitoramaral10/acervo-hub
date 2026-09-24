import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  CircleAlert,
  CircleCheck,
  CircleDashed,
  Film,
  CirclePause,
  FileCode2,
  Pencil,
  Lock,
  Plus,
  RefreshCw,
  Search,
  ServerOff,
  Trash2,
  Tv,
} from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { AddIndexerDialog } from '@/components/AddIndexerDialog'
import { CredentialsDialog } from '@/components/CredentialsDialog'
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
import { Badge, Skeleton, Switch, Tooltip } from '@/components/ui/misc'
import { type Indexer, api } from '@/lib/api'
import { formatAgo, formatCount } from '@/lib/format'
import { cn } from '@/lib/utils'

type Status = 'ok' | 'failing' | 'idle' | 'off'

function statusOf(indexer: Indexer): Status {
  if (!indexer.ativo) return 'off'
  if (indexer.saude.falhas_seguidas > 0) return 'failing'
  if (indexer.saude.ultimo_sucesso) return 'ok'
  return 'idle'
}

const STATUS = {
  ok: { label: 'Funcionando', tone: 'success', icon: CircleCheck },
  failing: { label: 'Com falha', tone: 'danger', icon: CircleAlert },
  idle: { label: 'Sem uso ainda', tone: 'neutral', icon: CircleDashed },
  off: { label: 'Desativado', tone: 'neutral', icon: CirclePause },
} as const

export function IndexersPage() {
  const indexers = useQuery({ queryKey: ['indexadores'], queryFn: api.indexers, refetchInterval: 30_000 })
  const [editing, setEditing] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  const list = indexers.data?.indexadores ?? []
  const failing = list.filter((indexer) => statusOf(indexer) === 'failing').length
  const working = list.filter((indexer) => statusOf(indexer) === 'ok').length
  const active = list.filter((indexer) => indexer.ativo).length

  return (
    <>
      <PageHeader
        title="Indexadores"
        description="Estado desde que o serviço subiu. Cada busca — do Sonarr, do Radarr ou daqui — atualiza esta tela."
        action={
          <div className="flex gap-2">
            <Button onClick={() => void indexers.refetch()} loading={indexers.isFetching && !indexers.isPending}>
              {!(indexers.isFetching && !indexers.isPending) && <RefreshCw aria-hidden="true" />}
              Atualizar
            </Button>
            <Button variant="primary" onClick={() => setAdding(true)}>
              <Plus aria-hidden="true" />
              Adicionar indexador
            </Button>
          </div>
        }
      />

      {indexers.isPending ? (
        <IndexersSkeleton />
      ) : indexers.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível carregar os indexadores.</p>
          <p className="text-sm text-content-muted">{indexers.error.message}</p>
          <Button onClick={() => void indexers.refetch()}>Tentar novamente</Button>
        </div>
      ) : list.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <ServerOff className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nenhum indexador ainda</p>
          <p className="max-w-md text-sm text-content-muted">
            Escolha um tracker no catálogo de definições, ou aponte para qualquer endpoint Torznab.
          </p>
          <Button variant="primary" onClick={() => setAdding(true)}>
            <Plus aria-hidden="true" />
            Adicionar indexador
          </Button>
        </div>
      ) : (
        <>
          <dl className="mb-6 grid grid-cols-3 gap-3 sm:max-w-lg">
            <Stat label="Ativos" value={active} />
            <Stat label="Funcionando" value={working} tone={working > 0 ? 'success' : undefined} />
            <Stat label="Com falha" value={failing} tone={failing > 0 ? 'danger' : undefined} />
          </dl>
          <ul className="grid gap-4 lg:grid-cols-2">
            {list.map((indexer) => (
              <li key={indexer.nome}>
                <IndexerCard indexer={indexer} onEdit={() => setEditing(indexer.nome)} />
              </li>
            ))}
          </ul>
        </>
      )}

      <CredentialsDialog name={editing} onClose={() => setEditing(null)} />
      <AddIndexerDialog open={adding} onClose={() => setAdding(false)} />
    </>
  )
}

function Stat({ label, value, tone }: { label: string; value: number; tone?: 'success' | 'danger' }) {
  return (
    <div className="rounded-lg border border-border bg-surface px-4 py-3">
      <dt className="text-xs text-content-muted">{label}</dt>
      <dd
        className={cn(
          'mt-0.5 text-2xl font-semibold tracking-tight tabular-nums',
          tone === 'success' && 'text-success',
          tone === 'danger' && 'text-danger',
        )}
      >
        {value}
      </dd>
    </div>
  )
}

function IndexerCard({ indexer, onEdit }: { indexer: Indexer; onEdit: () => void }) {
  const queryClient = useQueryClient()
  const status = STATUS[statusOf(indexer)]
  const test = useMutation({
    mutationFn: () => api.test(indexer.nome),
    onSuccess: (result) => {
      if (result.ok) toast.success(`${indexer.nome}: ${formatCount(result.resultados)} resultados recentes`)
      else toast.error(`${indexer.nome}: ${result.erro ?? 'o teste falhou'}`)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['indexadores'] }),
  })
  const toggle = useMutation({
    mutationFn: (ativo: boolean) => api.setEnabled(indexer.nome, ativo),
    onSuccess: (_, ativo) =>
      toast.success(ativo ? `${indexer.nome} ativado` : `${indexer.nome} desativado — não é servido nem sincronizado`),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['indexadores'] }),
  })
  const [confirming, setConfirming] = useState(false)
  const remove = useMutation({
    mutationFn: () => api.removeIndexer(indexer.nome),
    onSuccess: () => {
      toast.success(`${indexer.nome} removido`)
      setConfirming(false)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ['indexadores'] })
      void queryClient.invalidateQueries({ queryKey: ['catalogo'] })
    },
  })
  const health = indexer.saude
  const StatusIcon = status.icon

  return (
    <article
      aria-labelledby={`indexador-${indexer.nome}`}
      className={cn(
        'flex h-full flex-col gap-4 rounded-lg border border-border bg-surface p-5 transition-colors hover:border-border-strong',
        !indexer.ativo && 'bg-bg',
      )}
    >
      <header className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h2 id={`indexador-${indexer.nome}`} className="truncate text-base font-semibold tracking-tight">
              {indexer.nome}
            </h2>
            {indexer.origem === 'config' && (
              <Tooltip content="Veio do config.toml. O que você muda aqui vale por cima do arquivo, que não é alterado.">
                <span className="inline-flex text-content-subtle">
                  <FileCode2 className="size-3.5" aria-hidden="true" />
                  <span className="sr-only">do config.toml</span>
                </span>
              </Tooltip>
            )}
            {indexer.privado && (
              <Tooltip content="Tracker privado: login e download passam pela sessão guardada aqui.">
                <span className="inline-flex text-content-subtle">
                  <Lock className="size-3.5" aria-hidden="true" />
                  <span className="sr-only">privado</span>
                </span>
              </Tooltip>
            )}
          </div>
          <div className="mt-2 flex flex-wrap gap-1.5">
            {indexer.modos.filmes && (
              <Badge>
                <Film aria-hidden="true" /> Filmes
              </Badge>
            )}
            {indexer.modos.series && (
              <Badge>
                <Tv aria-hidden="true" /> Séries
              </Badge>
            )}
            {indexer.modos.busca && (
              <Badge>
                <Search aria-hidden="true" /> Busca livre
              </Badge>
            )}
            {indexer.ativo && <Badge>{indexer.categorias.length} categorias</Badge>}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          <Badge tone={status.tone} className="py-1">
            <StatusIcon aria-hidden="true" />
            {status.label}
          </Badge>
          <Switch
            checked={indexer.ativo}
            disabled={toggle.isPending}
            onCheckedChange={(ativo) => toggle.mutate(ativo)}
            aria-label={indexer.ativo ? `Desativar ${indexer.nome}` : `Ativar ${indexer.nome}`}
          />
        </div>
      </header>

      {!indexer.ativo ? (
        <p className="border-t border-border pt-4 text-sm text-content-muted">
          Desativado: não é servido ao Sonarr e ao Radarr nem entra na busca. Ative para voltar.
        </p>
      ) : (
      <dl className="grid grid-cols-3 gap-3 border-t border-border pt-4 text-sm">
        <div>
          <dt className="text-xs text-content-subtle">Último sucesso</dt>
          <dd className="mt-0.5 font-medium">{formatAgo(health.ultimo_sucesso)}</dd>
        </div>
        <div>
          <dt className="text-xs text-content-subtle">Resultados</dt>
          <dd className="mt-0.5 font-medium tabular-nums">{formatCount(health.resultados)}</dd>
        </div>
        <div>
          <dt className="text-xs text-content-subtle">Falhas seguidas</dt>
          <dd className={cn('mt-0.5 font-medium tabular-nums', health.falhas_seguidas > 0 && 'text-danger')}>
            {health.falhas_seguidas}
          </dd>
        </div>
      </dl>
      )}

      {health.falhas_seguidas > 0 && health.ultimo_erro && (
        <p className="rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
          <span className="font-medium">{formatAgo(health.ultima_falha)}:</span> {health.ultimo_erro}
        </p>
      )}

      <footer className="mt-auto flex flex-wrap gap-2">
        <Button
          size="sm"
          onClick={() => test.mutate()}
          loading={test.isPending}
          disabled={!indexer.ativo}
          aria-label={`Testar ${indexer.nome}`}
        >
          {!test.isPending && <RefreshCw aria-hidden="true" />}
          {test.isPending ? 'Testando…' : 'Testar'}
        </Button>
        {indexer.editavel && (
          <Button size="sm" variant="ghost" onClick={onEdit} aria-label={`Editar ${indexer.nome}`}>
            <Pencil aria-hidden="true" />
            Editar
          </Button>
        )}
        <Button
          size="sm"
          variant="ghost"
          className="ml-auto hover:text-danger"
          onClick={() => setConfirming(true)}
          aria-label={`Remover ${indexer.nome}`}
        >
          <Trash2 aria-hidden="true" />
          Remover
        </Button>
      </footer>

      <Dialog open={confirming} onOpenChange={setConfirming}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Remover {indexer.nome}?</DialogTitle>
            <DialogDescription>
              Sai do acervo-hub junto com as credenciais guardadas pela tela. O cadastro dele no Sonarr e no Radarr só
              some na próxima sincronização.
              {indexer.origem === 'config' &&
                ' Ele veio do config.toml: o arquivo não muda, e dá para trazê-lo de volta pelo catálogo.'}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirming(false)}>
              Cancelar
            </Button>
            <Button variant="danger" loading={remove.isPending} onClick={() => remove.mutate()}>
              Remover
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </article>
  )
}

function IndexersSkeleton() {
  return (
    <div aria-busy="true" aria-label="Carregando indexadores">
      <div className="mb-6 grid grid-cols-3 gap-3 sm:max-w-lg">
        {[0, 1, 2].map((key) => (
          <Skeleton key={key} className="h-[62px] rounded-lg" />
        ))}
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        {[0, 1].map((key) => (
          <Skeleton key={key} className="h-[214px] rounded-lg" />
        ))}
      </div>
    </div>
  )
}
