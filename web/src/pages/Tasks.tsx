import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, CircleAlert, CircleCheck, Clock, Loader2, Play } from 'lucide-react'
import { Fragment, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { CycleCard } from '@/components/CycleCard'
import { PageHeader } from '@/components/PageHeader'
import { WatchedCard } from '@/components/WatchedCard'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge, Skeleton, Tooltip } from '@/components/ui/misc'
import {
  type CycleReport,
  type MetadataReport,
  type Task,
  type TaskRun,
  type TasksSection,
  type WatchedReport,
  api,
} from '@/lib/api'
import { formatAgo, formatDuration, formatInterval } from '@/lib/format'
import { cn } from '@/lib/utils'
import { useSection } from '@/pages/settings/Sections'

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

  // Intervalos e limite da busca moram na seção `tarefas` da configuração.
  const { query: schedule, save } = useSection('tarefas')
  const saveSchedule = (patch: Partial<TasksSection>) => {
    if (!schedule.data) return
    save.mutate({ ...schedule.data, ...patch })
  }

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
        description="O que o serviço roda sozinho: busca dos que faltam, RSS, importação, metadados, a limpeza — que apaga o seed que perdeu o hardlink com a biblioteca e o download sem dono — e, com o Jellyfin configurado, a remoção dos filmes já assistidos. O intervalo se muda aqui, em minutos (0 desliga o agendamento), e vale na hora; cada uma pode rodar agora, fora da hora."
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
          <Empty title="Nenhuma tarefa neste serviço" text="O serviço não registrou tarefas de fundo." />
        ) : (
          <>
            <ScheduledTable
              tasks={tasks.data.tarefas}
              pending={run.isPending ? run.variables : undefined}
              onRun={(id) => run.mutate(id)}
              intervals={schedule.data?.intervalos}
              onInterval={(id, minutes) =>
                schedule.data && saveSchedule({ intervalos: { ...schedule.data.intervalos, [id]: minutes } })
              }
            />
            {schedule.data && (
              <SearchLimit
                value={schedule.data.search_limit}
                saving={save.isPending}
                onSave={(search_limit) => saveSchedule({ search_limit })}
              />
            )}
          </>
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

/** Minutos inteiros de 0 a 30 dias, ou `null` se o texto não é isso. */
function parseMinutes(text: string): number | null {
  const value = Number(text)
  return text.trim() !== '' && Number.isInteger(value) && value >= 0 && value <= 43_200 ? value : null
}

/** Campo de minutos que grava ao sair dele ou no Enter, só se mudou. */
function MinutesInput({
  id,
  label,
  value,
  min = 0,
  onSave,
}: {
  id: string
  label: string
  value: number
  min?: number
  onSave: (value: number) => void
}) {
  const [text, setText] = useState(String(value))
  useEffect(() => setText(String(value)), [value])
  const commit = () => {
    const parsed = parseMinutes(text)
    if (parsed === null || parsed < min) {
      setText(String(value))
      return
    }
    if (parsed !== value) onSave(parsed)
  }
  return (
    <Input
      id={id}
      type="number"
      min={min}
      step={1}
      inputMode="numeric"
      aria-label={label}
      value={text}
      onChange={(event) => setText(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.key === 'Enter') {
          event.preventDefault()
          commit()
        }
        if (event.key === 'Escape') setText(String(value))
      }}
      className="h-8 w-20 tabular-nums"
    />
  )
}

function SearchLimit({ value, saving, onSave }: { value: number; saving: boolean; onSave: (value: number) => void }) {
  return (
    <div className="mt-3 flex flex-wrap items-center gap-2 text-sm text-content-muted">
      <label htmlFor="limite-busca">A busca agendada pega até</label>
      <MinutesInput id="limite-busca" label="Filmes por rodada da busca agendada" value={value} min={1} onSave={onSave} />
      <span>filmes por rodada; “rodar agora” busca todos.</span>
      {saving && <Loader2 className="size-3.5 animate-spin" aria-label="Salvando" />}
    </div>
  )
}

function ScheduledTable({
  tasks,
  pending,
  onRun,
  intervals,
  onInterval,
}: {
  tasks: Task[]
  pending: string | undefined
  onRun: (id: string) => void
  intervals: Record<string, number> | undefined
  onInterval: (id: string, minutes: number) => void
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
              Intervalo (min)
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
            <ScheduledRow
              key={task.id}
              task={task}
              pending={pending === task.id}
              onRun={() => onRun(task.id)}
              minutes={intervals?.[task.id] ?? task.intervalo_minutos}
              editable={intervals !== undefined}
              onInterval={(minutes) => onInterval(task.id, minutes)}
            />
          ))}
        </tbody>
      </table>
    </div>
  )
}

function ScheduledRow({
  task,
  pending,
  onRun,
  minutes,
  editable,
  onInterval,
}: {
  task: Task
  pending: boolean
  onRun: () => void
  minutes: number
  editable: boolean
  onInterval: (minutes: number) => void
}) {
  const last = task.ultima
  return (
    <tr className="transition-colors hover:bg-surface-raised/60">
      <td className="px-4 py-3 font-medium">{task.nome}</td>
      <td className="px-3 py-2 whitespace-nowrap text-content-muted">
        {editable ? (
          <MinutesInput
            id={`intervalo-${task.id}`}
            label={`Intervalo de ${task.nome}, em minutos (0 desliga)`}
            value={minutes}
            onSave={onInterval}
          />
        ) : (
          formatInterval(minutes)
        )}
      </td>
      <td className="px-3 py-3 whitespace-nowrap text-content-muted">
        {task.rodando ? <When iso={task.iniciada_em} /> : <When iso={last?.inicio} />}
      </td>
      <td className="px-3 py-3 text-right whitespace-nowrap tabular-nums text-content-muted">
        {last ? formatDuration(last.duracao_ms) : '—'}
      </td>
      <td className="px-3 py-3 whitespace-nowrap text-content-muted">
        {task.rodando ? (
          '—'
        ) : task.indisponivel ? (
          <Tooltip content={task.indisponivel}>
            <span className="inline-flex items-center gap-1 text-warning" tabIndex={0}>
              <CircleAlert className="size-3.5" aria-hidden="true" />
              parada
              <span className="sr-only">: {task.indisponivel}</span>
            </span>
          </Tooltip>
        ) : task.intervalo_minutos > 0 ? (
          <When iso={task.proxima} />
        ) : (
          'só manual'
        )}
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
            // Só a limpeza, os assistidos e os metadados têm relatório para abrir.
            const report = run.detalhe ? detail(run) : null
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
                      {report.tarefa === 'limpeza' ? (
                        <CycleCard report={report.report} />
                      ) : report.tarefa === 'metadados' ? (
                        <MetadataCard report={report.report} />
                      ) : (
                        <WatchedCard report={report.report} />
                      )}
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

type Detail =
  | { tarefa: 'limpeza'; report: CycleReport }
  | { tarefa: 'assistidos'; report: WatchedReport }
  | { tarefa: 'metadados'; report: MetadataReport }

function detail(run: TaskRun): Detail | null {
  if (run.tarefa === 'limpeza') return { tarefa: 'limpeza', report: run.detalhe as CycleReport }
  if (run.tarefa === 'assistidos') return { tarefa: 'assistidos', report: run.detalhe as WatchedReport }
  if (run.tarefa === 'metadados') return { tarefa: 'metadados', report: run.detalhe as MetadataReport }
  return null
}

/** O que a atualização de metadados mudou e o que falhou, filme a filme. */
function MetadataCard({ report }: { report: MetadataReport }) {
  return (
    <div className="grid gap-4 text-sm sm:grid-cols-2">
      <div>
        <h3 className="mb-2 font-semibold">Falharam ({report.falhas.length})</h3>
        {report.falhas.length === 0 ? (
          <p className="text-content-muted">Nenhuma falha.</p>
        ) : (
          <ul className="grid gap-1.5">
            {report.falhas.map((falha) => (
              <li key={falha.filme} className="flex items-start gap-1.5">
                <CircleAlert className="mt-0.5 size-3.5 shrink-0 text-danger" aria-hidden="true" />
                <span>
                  <span className="font-medium">{falha.filme}</span>
                  <span className="text-content-muted"> — {falha.erro}</span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div>
        <h3 className="mb-2 font-semibold">Atualizados ({report.atualizados.length})</h3>
        {report.atualizados.length === 0 ? (
          <p className="text-content-muted">Nada mudou.</p>
        ) : (
          <ul className="grid gap-1 text-content-muted">
            {report.atualizados.map((title) => (
              <li key={title}>{title}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
