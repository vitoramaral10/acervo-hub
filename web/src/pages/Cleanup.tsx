import { useMutation, useQuery } from '@tanstack/react-query'
import { CircleAlert, Clock, FlaskConical, Sparkles } from 'lucide-react'
import { toast } from 'sonner'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type CycleReport, api } from '@/lib/api'
import { formatAgo, formatCount } from '@/lib/format'

export function CleanupPage() {
  const last = useQuery({ queryKey: ['limpeza'], queryFn: api.lastCycle, refetchInterval: 60_000 })
  const simulate = useMutation({
    mutationFn: api.simulateCycle,
    onError: (error: Error) => toast.error(error.message),
  })

  return (
    <>
      <PageHeader
        title="Limpeza"
        description="O ciclo roda de hora em hora: tira da fila o que o gerenciador esqueceu e apaga o seed que perdeu o hardlink com a biblioteca — só nas categorias dos gerenciadores, com carência para tracker privado."
        action={
          <Button onClick={() => simulate.mutate()} loading={simulate.isPending}>
            {!simulate.isPending && <FlaskConical aria-hidden="true" />}
            {simulate.isPending ? 'Simulando…' : 'Simular agora'}
          </Button>
        }
      />

      {simulate.data && (
        <section className="mb-8" aria-labelledby="titulo-simulacao">
          <h2 id="titulo-simulacao" className="mb-3 text-sm font-semibold text-content-muted">
            Simulação de agora — nada foi alterado
          </h2>
          <CycleCard report={simulate.data} />
        </section>
      )}

      <section aria-labelledby="titulo-ultimo">
        <h2 id="titulo-ultimo" className="mb-3 text-sm font-semibold text-content-muted">
          Último ciclo
        </h2>
        {last.isPending ? (
          <Skeleton className="h-64 rounded-lg" />
        ) : last.isError ? (
          <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
            <p className="font-medium">Não foi possível ler o último ciclo.</p>
            <p className="text-sm text-content-muted">{last.error.message}</p>
            <Button onClick={() => void last.refetch()}>Tentar novamente</Button>
          </div>
        ) : last.data.ultimo ? (
          <CycleCard report={last.data.ultimo} />
        ) : (
          <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
            <Clock className="size-8 text-content-subtle" aria-hidden="true" />
            <p className="font-medium">Nenhum ciclo registrado ainda</p>
            <p className="max-w-md text-sm text-content-muted">
              O primeiro aparece aqui depois que o timer rodar. Enquanto isso, simule para ver o que ele faria.
            </p>
          </div>
        )}
      </section>
    </>
  )
}

function CycleCard({ report }: { report: CycleReport }) {
  const destructive = report.acoes.filter((action) => action.tipo !== 'strike')
  return (
    <article className="grid gap-5 rounded-lg border border-border bg-surface p-5">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-sm">
          <time dateTime={report.quando} title={new Date(report.quando).toLocaleString('pt-BR')} className="font-medium">
            {formatAgo(report.quando)}
          </time>
          <Badge>{report.modo === 'simulacao' ? 'simulação' : 'aplicado'}</Badge>
        </div>
        {report.abortado ? (
          <Badge tone="warning" className="py-1">
            <CircleAlert aria-hidden="true" /> Abortado por trava
          </Badge>
        ) : report.falharam ? (
          <Badge tone="danger" className="py-1">
            <CircleAlert aria-hidden="true" /> {report.falharam} falha(s)
          </Badge>
        ) : (
          <Badge tone="success" className="py-1">
            <Sparkles aria-hidden="true" /> {destructive.length === 0 ? 'Nada a limpar' : `${destructive.length} ação(ões)`}
          </Badge>
        )}
      </header>

      {report.abortado && (
        <p className="rounded-md bg-warning-bg px-3 py-2 text-sm text-warning">
          {report.abortado}. Nenhuma alteração foi feita — a leitura do mundo não era confiável.
        </p>
      )}

      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Fact label="Torrents" value={formatCount(report.torrents)} />
        <Fact label="Biblioteca" value={report.biblioteca} />
        <Fact label="A liberar" value={report.espaco} />
        <Fact label="Ilegíveis" value={formatCount(report.ilegiveis.length)} />
      </dl>

      <div className="grid gap-2">
        <h3 className="text-xs font-medium text-content-subtle">Gerenciadores lidos</h3>
        <ul className="flex flex-wrap gap-2">
          {report.instancias.map((instance) => (
            <li key={instance.nome}>
              <Badge tone={instance.erro ? 'danger' : 'neutral'} className="py-1">
                {instance.nome}:{' '}
                {instance.erro ?? `${formatCount(instance.fila)} na fila, ${formatCount(instance.obras)} obras`}
              </Badge>
            </li>
          ))}
        </ul>
      </div>

      {report.acoes.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-medium text-content-subtle">Ações</h3>
          <ul className="divide-y divide-border rounded-md border border-border text-sm">
            {report.acoes.map((action, index) => (
              <li key={`${action.titulo}-${index}`} className="flex items-center justify-between gap-3 px-3 py-2">
                <span className="min-w-0 truncate" title={action.titulo}>
                  {action.instancia && <span className="text-content-subtle">[{action.instancia}] </span>}
                  {action.titulo}
                </span>
                <span className="shrink-0 text-xs text-content-muted">{action.detalhe}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {report.pulados.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-medium text-content-subtle">Pulados, e por quê</h3>
          <ul className="grid gap-1 text-sm">
            {report.pulados.map((skipped) => (
              <li key={skipped.motivo} className="flex gap-3">
                <span className="w-12 shrink-0 text-right font-medium tabular-nums">{formatCount(skipped.quantos)}</span>
                <span className="text-content-muted">{skipped.motivo}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </article>
  )
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md bg-surface-raised px-3 py-2">
      <dt className="text-xs text-content-subtle">{label}</dt>
      <dd className="mt-0.5 font-medium tabular-nums">{value}</dd>
    </div>
  )
}
