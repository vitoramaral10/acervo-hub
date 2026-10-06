import { type UseQueryResult, useQuery } from '@tanstack/react-query'
import { ArrowRight, CalendarDays, CircleAlert, CircleCheck, Download, HardDrive, ListChecks, Server } from 'lucide-react'
import type { ReactNode } from 'react'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type CalendarItem, type QueueItem, api, library } from '@/lib/api'
import { formatAgo, formatCount, formatSize } from '@/lib/format'
import { isStuck, queueStatus, stuckFirst } from '@/lib/queue'
import { episodeCode } from '@/lib/seriesFormat'
import { cn } from '@/lib/utils'
import { iso } from '@/pages/Calendar'

/** Abaixo disto, a pasta ganha destaque: um filme em 2160p passa de 50 GiB. */
const LOW_DISK = 20 * 1024 ** 3
/** Indexador com tantas falhas seguidas entra na lista de atenção. */
const FAILURES = 2
const UPCOMING_DAYS = 7

/** Cartão de um bloco da tela, com o atalho para a tela detalhada. */
function Panel({
  title,
  icon: Icon,
  href,
  linkLabel,
  summary,
  className,
  children,
}: {
  title: string
  icon: typeof Download
  href: string
  linkLabel: string
  summary?: ReactNode
  className?: string
  children: ReactNode
}) {
  return (
    <section className={cn('flex flex-col gap-4 rounded-lg border border-border bg-surface p-5', className)}>
      <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <h2 className="flex items-center gap-2 text-sm font-semibold">
          <Icon className="size-4 text-content-subtle" aria-hidden="true" />
          {title}
        </h2>
        {summary && <p className="text-sm text-content-muted tabular-nums">{summary}</p>}
      </header>
      <div className="flex-1">{children}</div>
      <Button asChild variant="ghost" size="sm" className="-mb-2 self-start">
        <a href={href}>
          {linkLabel}
          <ArrowRight aria-hidden="true" />
        </a>
      </Button>
    </section>
  )
}

/** Carregando, erro e dado: o mesmo tratamento em todo bloco. */
function Loaded<T>({
  query,
  rows = 2,
  children,
}: {
  query: UseQueryResult<T>
  rows?: number
  children: (data: T) => ReactNode
}) {
  if (query.isPending) {
    return (
      <div aria-busy="true" aria-label="Carregando" className="grid gap-2">
        {Array.from({ length: rows }, (_, key) => (
          <Skeleton key={key} className="h-10" />
        ))}
      </div>
    )
  }
  if (query.isError) {
    return (
      <div role="alert" className="flex flex-wrap items-center gap-3 text-sm">
        <p className="text-content-muted">Não foi possível carregar: {query.error.message}</p>
        <Button size="sm" onClick={() => void query.refetch()}>
          Tentar novamente
        </Button>
      </div>
    )
  }
  return children(query.data)
}

function Nothing({ ok, children }: { ok?: boolean; children: ReactNode }) {
  const Icon = ok ? CircleCheck : CircleAlert
  return (
    <p className="flex items-center gap-2 text-sm text-content-muted">
      <Icon className={cn('size-4 shrink-0', ok ? 'text-success' : 'text-content-subtle')} aria-hidden="true" />
      {children}
    </p>
  )
}

const titleOf = (item: QueueItem) => item.filme ?? (item.tipo === 'serie' ? 'Série' : `Filme ${item.filme_id}`)

function DownloadRow({ item }: { item: QueueItem }) {
  const status = queueStatus(item)
  const progress = item.progresso ?? 0
  const stuck = isStuck(item)
  return (
    <li className={cn('grid gap-1.5 py-3', stuck && '-mx-2 rounded-md bg-warning-bg px-2')}>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <p className="min-w-0 truncate text-sm font-medium">{titleOf(item)}</p>
        <Badge tone={status.tone}>{status.label}</Badge>
      </div>
      <div className="flex items-center gap-3">
        <div
          className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-raised"
          role="progressbar"
          aria-label={`Progresso de ${titleOf(item)}`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(progress * 100)}
        >
          <div className="h-full rounded-full bg-accent" style={{ width: `${progress * 100}%` }} />
        </div>
        <span className="w-10 text-right text-xs text-content-muted tabular-nums">{Math.floor(progress * 100)}%</span>
      </div>
      <p className={cn('text-xs tabular-nums', stuck ? 'text-warning' : 'text-content-subtle')}>
        {item.mensagem ??
          (item.velocidade ? `${formatSize(item.velocidade)}/s` : `pego ${formatAgo(item.pego_em)} · ${item.indexador}`)}
      </p>
    </li>
  )
}

function DownloadsPanel() {
  const queue = useQuery({ queryKey: ['fila'], queryFn: library.queue, refetchInterval: 5_000 })
  const items = queue.data ? stuckFirst(queue.data.fila) : []
  const stuck = items.filter(isStuck).length
  const speed = items.reduce((sum, item) => sum + (item.velocidade ?? 0), 0)
  return (
    <Panel
      title="Downloads"
      icon={Download}
      href="#atividade"
      linkLabel="Abrir a fila"
      className="lg:col-span-2"
      summary={
        queue.data &&
        items.length > 0 && (
          <>
            {formatCount(items.length)} na fila · {formatSize(speed)}/s
            {stuck > 0 && <span className="font-medium text-warning"> · {stuck} travado(s)</span>}
          </>
        )
      }
    >
      <Loaded query={queue} rows={3}>
        {() =>
          items.length === 0 ? (
            <Nothing ok>Nada baixando agora.</Nothing>
          ) : (
            <ul className="divide-y divide-border">
              {items.slice(0, 6).map((item) => (
                <DownloadRow key={item.id} item={item} />
              ))}
              {items.length > 6 && (
                <li className="py-3 text-xs text-content-subtle">e mais {items.length - 6} na fila.</li>
              )}
            </ul>
          )
        }
      </Loaded>
    </Panel>
  )
}

function DiskPanel() {
  const options = useQuery({ queryKey: ['biblioteca-opcoes'], queryFn: library.options, refetchInterval: 60_000 })
  return (
    <Panel title="Espaço em disco" icon={HardDrive} href="#biblioteca" linkLabel="Pastas da biblioteca">
      <Loaded query={options}>
        {({ pastas }) =>
          pastas.length === 0 ? (
            <Nothing>Nenhuma pasta de biblioteca configurada.</Nothing>
          ) : (
            <ul className="grid gap-3">
              {pastas.map((folder) => {
                const low = folder.livre != null && folder.livre < LOW_DISK
                return (
                  <li key={folder.caminho} className="flex items-baseline justify-between gap-4">
                    <p className="min-w-0 font-mono text-xs break-all text-content-muted">{folder.caminho}</p>
                    {folder.livre == null ? (
                      <span className="shrink-0 text-sm text-content-subtle">indisponível</span>
                    ) : (
                      <span
                        className={cn('shrink-0 text-sm font-medium tabular-nums', low && 'text-danger')}
                        title={low ? 'Pouco espaço livre' : undefined}
                      >
                        {formatSize(folder.livre)} livres
                      </span>
                    )}
                  </li>
                )
              })}
            </ul>
          )
        }
      </Loaded>
    </Panel>
  )
}

function IndexersPanel() {
  const indexers = useQuery({ queryKey: ['indexadores'], queryFn: api.indexers, refetchInterval: 30_000 })
  return (
    <Panel title="Indexadores" icon={Server} href="#indexadores" linkLabel="Ver indexadores">
      <Loaded query={indexers}>
        {({ indexadores }) => {
          const now = Date.now()
          const ailing = indexadores
            .filter((item) => item.ativo)
            .map((item) => ({
              item,
              waiting: item.saude.em_espera_ate != null && Date.parse(item.saude.em_espera_ate) > now,
            }))
            .filter(({ item, waiting }) => waiting || item.saude.falhas_seguidas >= FAILURES)
          if (ailing.length === 0) return <Nothing ok>Todos respondendo.</Nothing>
          return (
            <ul className="divide-y divide-border">
              {ailing.map(({ item, waiting }) => (
                <li key={item.nome} className="grid gap-1 py-2.5 first:pt-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="text-sm font-medium">{item.nome}</p>
                    {waiting && <Badge tone="warning">Em espera</Badge>}
                    {item.saude.falhas_seguidas >= FAILURES && (
                      <Badge tone="danger">{item.saude.falhas_seguidas} falhas seguidas</Badge>
                    )}
                  </div>
                  {item.saude.ultimo_erro && (
                    <p className="line-clamp-2 text-xs text-content-subtle">{item.saude.ultimo_erro}</p>
                  )}
                </li>
              ))}
            </ul>
          )
        }}
      </Loaded>
    </Panel>
  )
}

function TaskPanel() {
  const history = useQuery({ queryKey: ['tarefas-historico'], queryFn: api.taskHistory, refetchInterval: 60_000 })
  return (
    <Panel title="Tarefas" icon={ListChecks} href="#tarefas" linkLabel="Ver tarefas">
      <Loaded query={history}>
        {({ historico }) => {
          const failure = historico
            .filter((run) => !run.ok)
            .sort((a, b) => Date.parse(b.fim) - Date.parse(a.fim))[0]
          if (!failure) return <Nothing ok>Nenhuma falha no histórico recente.</Nothing>
          return (
            <div className="grid gap-1">
              <div className="flex flex-wrap items-center gap-2">
                <p className="text-sm font-medium">{failure.nome}</p>
                <Badge tone="danger">Falhou {formatAgo(failure.fim)}</Badge>
              </div>
              <p className="line-clamp-3 text-xs text-content-subtle">{failure.resumo}</p>
            </div>
          )
        }}
      </Loaded>
    </Panel>
  )
}

function upcomingLabel(item: CalendarItem): string {
  return item.tipo === 'episodio'
    ? `${item.serie} ${episodeCode(item.temporada, item.numero)}`
    : `${item.filme}${item.ano ? ` (${item.ano})` : ''}`
}

const dayFormat = new Intl.DateTimeFormat('pt-BR', { weekday: 'short', day: 'numeric', month: 'short' })

function dayOf(day: string): string {
  const [year = 0, month = 1, date = 1] = day.split('-').map(Number)
  return dayFormat.format(new Date(year, month - 1, date))
}

function MissingPanel() {
  const missing = useQuery({ queryKey: ['faltando', 'todos'], queryFn: () => library.missing(), refetchInterval: 60_000 })
  const from = new Date()
  const to = new Date(from.getFullYear(), from.getMonth(), from.getDate() + UPCOMING_DAYS - 1)
  const de = iso(from)
  const ate = iso(to)
  const upcoming = useQuery({ queryKey: ['calendario', de, ate], queryFn: () => library.calendar(de, ate) })
  return (
    <Panel
      title="Faltando e lançamentos"
      icon={CalendarDays}
      href="#faltando"
      linkLabel="Ver o que falta"
    >
      <div className="grid gap-5">
        <div>
          <h3 className="mb-2 text-xs font-medium text-content-subtle uppercase">Faltando</h3>
          <Loaded query={missing} rows={1}>
            {({ total, itens }) =>
              total === 0 ? (
                <Nothing ok>Nada faltando.</Nothing>
              ) : (
                <p className="text-sm">
                  <span className="text-2xl font-semibold tabular-nums">{formatCount(total)}</span>{' '}
                  <span className="text-content-muted">
                    {total === 1 ? 'item' : 'itens'} fora do disco
                    {itens.some((item) => item.baixando) && ` · ${itens.filter((item) => item.baixando).length} baixando`}
                  </span>
                </p>
              )
            }
          </Loaded>
        </div>
        <div>
          <h3 className="mb-2 text-xs font-medium text-content-subtle uppercase">
            Próximos {UPCOMING_DAYS} dias{' '}
            <a href="#calendario" className="normal-case underline underline-offset-2 hover:text-content">
              calendário
            </a>
          </h3>
          <Loaded query={upcoming} rows={2}>
            {({ total, itens }) =>
              total === 0 ? (
                <Nothing>Nenhum lançamento previsto.</Nothing>
              ) : (
                <>
                  <p className="mb-2 text-sm text-content-muted">
                    <span className="text-2xl font-semibold text-content tabular-nums">{formatCount(total)}</span>{' '}
                    {total === 1 ? 'lançamento' : 'lançamentos'}
                  </p>
                  <ul className="grid gap-1 text-sm">
                    {[...itens]
                      .sort((a, b) => a.data.localeCompare(b.data))
                      .slice(0, 4)
                      .map((item) => (
                        <li
                          key={`${item.tipo}-${item.tipo === 'episodio' ? item.episodio_id : item.filme_id}-${item.data}`}
                          className="flex items-baseline justify-between gap-3"
                        >
                          <span className="min-w-0 truncate">{upcomingLabel(item)}</span>
                          <span className="shrink-0 text-xs text-content-subtle">{dayOf(item.data)}</span>
                        </li>
                      ))}
                  </ul>
                </>
              )
            }
          </Loaded>
        </div>
      </div>
    </Panel>
  )
}

export function HomePage() {
  return (
    <>
      <PageHeader
        title="Painel"
        description="O que está baixando, o que travou e quanto disco sobra — num olhar."
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <DownloadsPanel />
        <DiskPanel />
        <IndexersPanel />
        <TaskPanel />
        <MissingPanel />
      </div>
    </>
  )
}
