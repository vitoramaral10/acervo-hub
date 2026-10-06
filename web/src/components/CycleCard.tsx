import { CircleAlert, Sparkles } from 'lucide-react'
import { Badge } from '@/components/ui/misc'
import type { CycleReport } from '@/lib/api'
import { formatAgo, formatCount } from '@/lib/format'

/** O relatório de um ciclo de limpeza: o que foi lido, o que se fez e o que ficou de fora. */
export function CycleCard({ report }: { report: CycleReport }) {
  const destructive = report.acoes.filter((action) => action.tipo !== 'strike')
  return (
    <article className="grid gap-5 rounded-lg border border-border bg-surface p-5">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-sm">
          <time dateTime={report.quando} title={new Date(report.quando).toLocaleString('pt-BR')} className="font-medium">
            {formatAgo(report.quando)}
          </time>
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

      {report.acoes.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-medium text-content-subtle">Ações</h3>
          <ul className="divide-y divide-border rounded-md border border-border text-sm">
            {report.acoes.map((action, index) => (
              <li key={`${action.titulo}-${index}`} className="flex items-center justify-between gap-3 px-3 py-2">
                <span className="min-w-0 truncate" title={action.titulo}>
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
