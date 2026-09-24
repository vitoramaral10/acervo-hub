import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleAlert, CircleCheck, Film, RefreshCw, Tv, Unplug } from 'lucide-react'
import { toast } from 'sonner'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type SyncAction, type SyncReport, api } from '@/lib/api'
import { cn } from '@/lib/utils'

const ACTION = {
  criar: { label: 'Criar', tone: 'accent' },
  atualizar: { label: 'Atualizar', tone: 'warning' },
  remover: { label: 'Remover', tone: 'danger' },
  manter: { label: 'Em dia', tone: 'neutral' },
} as const

export function AppsPage() {
  const queryClient = useQueryClient()
  const apps = useQuery({ queryKey: ['aplicativos'], queryFn: api.apps })
  // A prévia roda sozinha ao abrir: é leitura, e é o que a tela existe para mostrar.
  const preview = useQuery({ queryKey: ['sincronizacao'], queryFn: () => api.sync(false) })
  const apply = useMutation({
    mutationFn: () => api.sync(true),
    onSuccess: (report) => {
      queryClient.setQueryData(['sincronizacao'], { ...report, aplicado: false })
      if (report.falhas > 0) toast.error(`Sincronização com ${report.falhas} falha(s) — veja abaixo`)
      else toast.success('Sonarr e Radarr sincronizados')
      void queryClient.invalidateQueries({ queryKey: ['sincronizacao'] })
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const reachable = preview.data?.instancias.filter((instance) => !instance.erro).length ?? 0
  const pending =
    preview.data?.instancias.reduce(
      (total, instance) => total + instance.acoes.filter((action) => action.acao !== 'manter').length,
      0,
    ) ?? 0

  return (
    <>
      <PageHeader
        title="Aplicativos"
        description="Os gerenciadores que usam os indexadores daqui. Sincronizar cadastra, atualiza e remove os indexadores (acervo-hub) neles — os de outras origens ficam intocados."
        action={
          <div className="flex gap-2">
            <Button onClick={() => void preview.refetch()} loading={preview.isFetching && !apply.isPending}>
              {!(preview.isFetching && !apply.isPending) && <RefreshCw aria-hidden="true" />}
              Conferir de novo
            </Button>
            <Button
              variant="primary"
              onClick={() => apply.mutate()}
              loading={apply.isPending}
              disabled={!preview.data || pending === 0}
            >
              {apply.isPending
                ? 'Sincronizando…'
                : pending > 0
                  ? `Sincronizar (${pending})`
                  : preview.data && reachable === 0
                    ? 'Nenhum gerenciador alcançável'
                    : 'Tudo em dia'}
            </Button>
          </div>
        }
      />

      {apps.data && (
        <p className="mb-6 text-sm text-content-muted">
          Endereço cadastrado nos gerenciadores:{' '}
          <code className="rounded-sm bg-surface-raised px-1.5 py-0.5 font-mono text-xs text-content">
            {apps.data.aplicativos.endereco_publico ?? 'não configurado (server.public_url)'}
          </code>
        </p>
      )}

      {preview.isPending ? (
        <div className="grid gap-4 lg:grid-cols-2" aria-busy="true" aria-label="Conferindo">
          {[0, 1].map((key) => (
            <Skeleton key={key} className="h-48 rounded-lg" />
          ))}
        </div>
      ) : preview.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível conferir os gerenciadores.</p>
          <p className="text-sm text-content-muted">{preview.error.message}</p>
          <Button onClick={() => void preview.refetch()}>Tentar novamente</Button>
        </div>
      ) : preview.data.instancias.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <Unplug className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nenhum gerenciador configurado</p>
          <p className="max-w-md text-sm text-content-muted">
            Liste o Sonarr e o Radarr em <code className="font-mono">[[instances]]</code> no{' '}
            <code className="font-mono">config.toml</code>.
          </p>
        </div>
      ) : (
        <ul className="grid gap-4 lg:grid-cols-2">
          {preview.data.instancias.map((instance) => (
            <li key={instance.nome}>
              <InstanceCard instance={instance} url={apps.data?.aplicativos.instancias.find((i) => i.nome === instance.nome)?.url} />
            </li>
          ))}
        </ul>
      )}
    </>
  )
}

function InstanceCard({ instance, url }: { instance: SyncReport['instancias'][number]; url?: string }) {
  const Icon = instance.tipo === 'filmes' ? Film : Tv
  return (
    <article className="flex h-full flex-col gap-4 rounded-lg border border-border bg-surface p-5">
      <header className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <span className="grid size-9 place-items-center rounded-md bg-surface-raised text-content-muted">
            <Icon className="size-4" aria-hidden="true" />
          </span>
          <div className="min-w-0">
            <h2 className="text-base font-semibold tracking-tight">{instance.nome}</h2>
            {url && <p className="truncate font-mono text-xs text-content-subtle">{url}</p>}
          </div>
        </div>
        {instance.erro ? (
          <Badge tone="danger" className="py-1">
            <CircleAlert aria-hidden="true" /> Inalcançável
          </Badge>
        ) : (
          <Badge tone="success" className="py-1">
            <CircleCheck aria-hidden="true" /> Conectado
          </Badge>
        )}
      </header>
      {instance.erro ? (
        <p className="rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">{instance.erro}</p>
      ) : instance.acoes.length === 0 ? (
        <p className="text-sm text-content-muted">Nenhum indexador com categorias para este gerenciador.</p>
      ) : (
        <ul className="divide-y divide-border rounded-md border border-border">
          {instance.acoes.map((action) => (
            <ActionRow key={action.indexador} action={action} />
          ))}
        </ul>
      )}
    </article>
  )
}

function ActionRow({ action }: { action: SyncAction }) {
  const meta = ACTION[action.acao]
  return (
    <li className="flex items-center justify-between gap-3 px-3 py-2.5 text-sm">
      <div className="min-w-0">
        <p className={cn('truncate font-medium', action.acao === 'manter' && 'text-content-muted')}>
          {action.indexador}
        </p>
        {action.categorias.length > 0 && (
          <p className="font-mono text-xs text-content-subtle tabular-nums">{action.categorias.join(', ')}</p>
        )}
        {action.falha && <p className="text-xs text-danger">{action.falha}</p>}
      </div>
      <Badge tone={meta.tone}>{meta.label}</Badge>
    </li>
  )
}
