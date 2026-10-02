import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, CircleAlert, CircleCheck, Clock, Loader2, Play } from 'lucide-react'
import { Fragment, useState } from 'react'
import { toast } from 'sonner'
import { CycleCard } from '@/components/CycleCard'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton, Tooltip } from '@/components/ui/misc'
import { type CycleReport, type Task, type TaskRun, api } from '@/lib/api'
import { formatAgo, formatDuration, formatInterval } from '@/lib/format'
import { cn } from '@/lib/utils'

// Enquanto alguma tarefa roda, a tela acompanha de perto; parada, só confere de vez em quando.
const FAST = 3_000
const SLOW = 15_000

export function TasksPage() {
  const queryClient = useQueryClient()
  const tasks = useQuery({
    queryKey: ['tarefas'],
    queryFn: api.tasks,
    refetchInterval: (query) => (query.state.data?.tarefas.some((task) => task.rodando) ? FAST : SLOW),
  })
  const anyRunning = tasks.data?.tarefas.some((task) => task.rodando) ?? false
  const history = useQuery({
    queryKey: ['tarefas-historico'],
    queryFn: api.taskHistory,
    refetchInterval: anyRunning ? FAST : SLOW,
  })

  const run = useMutation({
    mutationFn: api.runTask,
    onSuccess: (result, id) => {
      if (!result.iniciada) toast.info('Essa tarefa já está rodando')
      void queryClient.invalidateQueries({ queryKey: ['tarefas'] })
      // A busca dos que faltam é a mesma do botão da tela de filmes.
      if (id === 'busca') void queryClient.invalidateQueries({ queryKey: ['filmes-busca'] })
    },
    onError: (error: Error) => toast.error(error.message),
  })

  return (
    <>
      <PageHeader
        title="Tarefas"
        description="O que o serviço roda sozinho: busca dos que faltam, RSS, importação, metadados e a limpeza — que tira da fila o que o gerenciador esqueceu e apaga o seed que perdeu o hardlink com a biblioteca. Cada uma pode rodar agora, fora da hora."
      />

      <section className="mb-10" aria-labelledby="titulo-agendadas">
        <h2 id="titulo-agendadas" className="mb-3 text-sm font-semibold text-content-muted">
          Agendadas
        </h2>
        {tasks.isPending ? (
          <Skeleton className="h-56 rounded-lg" />
        ) : tasks.isError ? (
          <Failure what="as tarefas" message={tasks.error.message} onRetry={() => void tasks.refetch()} />
        ) : tasks.data.tarefas.length === 0 ? (
          <Empty title="Nenhuma tarefa neste serviço" text="Sem banco de dados nem configuração da limpeza, não há o que agendar." />
        ) : (
          <ScheduledTable
            tasks={tasks.data.tarefas}
            pending={run.isPending ? run.variables : undefined}
            onRun={(id) => run.mutate(id)}
          />
        )}
      </section>

      <section aria-labelledby="titulo-historico">
        <h2 id="titulo-historico" className="mb-3 text-sm font-semibold text-content-muted">
          Histórico
        </h2>
        {history.isPending ? (
          <Skeleton className="h-64 rounded-lg" />
        ) : history.isError ? (
          <Failure what="o histórico" message={history.error.message} onRetry={() => void history.refetch()} />
        ) : history.data.historico.length === 0 ? (
          <Empty title="Nenhuma execução registrada ainda" text="Cada execução aparece aqui ao terminar, a agendada e a de rodar agora." />
        ) : (
          <HistoryTable runs={history.data.historico} />
        )}
      </section>
    </>
  )
}

function Failure({ what, message, onRetry }: { what: string; message: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
      <p className="font-medium">Não foi possível ler {what}.</p>
      <p className="text-sm text-content-muted">{message}</p>
      <Button onClick={onRetry}>Tentar novamente</Button>
    </div>
  )
}

function Empty({ title, text }: { title: string; text: string }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
      <Clock className="size-8 text-content-subtle" aria-hidden="true" />
      <p className="font-medium">{title}</p>
      <p className="max-w-md text-sm text-content-muted">{text}</p>
    </div>
  )
}

function When({ iso }: { iso: string | null | undefined }) {
  if (!iso) return <span className="text-content-subtle">—</span>
  return (
    <time dateTime={iso} title={new Date(iso).toLocaleString('pt-BR')}>
      {formatAgo(iso)}
    </time>
  )
}

function ScheduledTable({
  tasks,
  pending,
  onRun,
}: {
  tasks: Task[]
  pending: string | undefined
  onRun: (id: string) => void
}) {
  return (
    <div className="overflow-x-auto rounded-lg border border-border bg-surface">
      <table className="w-full min-w-[720px] text-sm">
        <thead className="bg-surface-raised text-left text-xs text-content-muted">
          <tr>
            <th scope="col" className="px-4 py-2.5 font-medium">
              Nome
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Intervalo
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Última execução
            </th>
            <th scope="col" className="px-3 py-2.5 text-right font-medium">
              Duração
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Próxima
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Resumo
            </th>
            <th scope="col" className="w-12 px-3 py-2.5">
              <span className="sr-only">Rodar agora</span>
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {tasks.map((task) => (
            <ScheduledRow key={task.id} task={task} pending={pending === task.id} onRun={() => onRun(task.id)} />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function ScheduledRow({ task, pending, onRun }: { task: Task; pending: boolean; onRun: () => void }) {
  const last = task.ultima
  return (
    <tr className="transition-colors hover:bg-surface-raised/60">
      <td className="px-4 py-3 font-medium">{task.nome}</td>
      <td className="px-3 py-3 whitespace-nowrap text-content-muted">{formatInterval(task.intervalo_minutos)}</td>
      <td className="px-3 py-3 whitespace-nowrap text-content-muted">
        {task.rodando ? <When iso={task.iniciada_em} /> : <When iso={last?.inicio} />}
      </td>
      <td className="px-3 py-3 text-right whitespace-nowrap tabular-nums text-content-muted">
        {last ? formatDuration(last.duracao_ms) : '—'}
      </td>
      <td className="px-3 py-3 whitespace-nowrap text-content-muted">
        {task.rodando ? '—' : task.intervalo_minutos > 0 ? <When iso={task.proxima} /> : 'só manual'}
      </td>
      <td className="px-3 py-3">
        {task.rodando ? (
          <span className="inline-flex items-center gap-1.5 text-accent">
            <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
            Rodando{task.andamento ? ` — ${task.andamento}` : '…'}
          </span>
        ) : last ? (
          <span className={cn('inline-flex items-start gap-1.5', last.ok ? 'text-content' : 'text-danger')}>
            {last.ok ? (
              <CircleCheck className="mt-0.5 size-3.5 shrink-0 text-success" aria-label="ok" />
            ) : (
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-label="erro" />
            )}
            <span className="line-clamp-2 break-words" title={last.resumo}>
              {last.resumo}
            </span>
          </span>
        ) : (
          <span className="text-content-subtle">nunca rodou</span>
        )}
      </td>
      <td className="px-3 py-3 text-right">
        <Tooltip content={task.rodando ? 'Rodando' : 'Rodar agora'}>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={onRun}
            disabled={task.rodando}
            loading={pending || task.rodando}
            aria-label={`Rodar agora: ${task.nome}`}
          >
            {!pending && !task.rodando && <Play aria-hidden="true" />}
          </Button>
        </Tooltip>
      </td>
    </tr>
  )
}

function HistoryTable({ runs }: { runs: TaskRun[] }) {
  const [open, setOpen] = useState<number | null>(null)
  return (
    <div className="overflow-x-auto rounded-lg border border-border bg-surface">
      <table className="w-full min-w-[640px] text-sm">
        <thead className="bg-surface-raised text-left text-xs text-content-muted">
          <tr>
            <th scope="col" className="px-4 py-2.5 font-medium">
              Tarefa
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Início
            </th>
            <th scope="col" className="px-3 py-2.5 text-right font-medium">
              Duração
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Resultado
            </th>
            <th scope="col" className="px-3 py-2.5 font-medium">
              Resumo
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {runs.map((run) => {
            // Só a limpeza tem relatório para abrir.
            const report = run.tarefa === 'limpeza' && run.detalhe ? (run.detalhe as CycleReport) : null
            const expanded = report !== null && open === run.id
            return (
              <Fragment key={run.id}>
                <tr className="transition-colors hover:bg-surface-raised/60">
                  <td className="px-4 py-3 font-medium">
                    {report ? (
                      <button
                        type="button"
                        onClick={() => setOpen(expanded ? null : run.id)}
                        aria-expanded={expanded}
                        className="inline-flex items-center gap-1.5 rounded-sm text-left hover:text-accent focus-visible:outline-2 focus-visible:outline-ring"
                      >
                        <ChevronRight
                          className={cn('size-4 shrink-0 transition-transform', expanded && 'rotate-90')}
                          aria-hidden="true"
                        />
                        {run.nome || run.tarefa}
                      </button>
                    ) : (
                      <span className="pl-[22px]">{run.nome || run.tarefa}</span>
                    )}
                  </td>
                  <td className="px-3 py-3 whitespace-nowrap text-content-muted">
                    <When iso={run.inicio} />
                  </td>
                  <td className="px-3 py-3 text-right whitespace-nowrap tabular-nums text-content-muted">
                    {formatDuration(run.duracao_ms)}
                  </td>
                  <td className="px-3 py-3">
                    {run.ok ? (
                      <Badge tone="success">
                        <CircleCheck aria-hidden="true" /> ok
                      </Badge>
                    ) : (
                      <Badge tone="danger">
                        <CircleAlert aria-hidden="true" /> erro
                      </Badge>
                    )}
                  </td>
                  <td className="px-3 py-3">
                    <span className="line-clamp-2 break-words" title={run.resumo}>
                      {run.resumo}
                    </span>
                  </td>
                </tr>
                {expanded && (
                  <tr>
                    <td colSpan={5} className="bg-surface-raised/40 px-4 py-4">
                      <CycleCard report={report} />
                    </td>
                  </tr>
                )}
              </Fragment>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
