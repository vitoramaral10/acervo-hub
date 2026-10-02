import { CircleAlert } from 'lucide-react'
import type { WatchedReport } from '@/lib/api'

function label(titulo: string, ano: number | null) {
  return ano ? `${titulo} (${ano})` : titulo
}

/** O detalhe de uma execução de "Apagar assistidos": o que saiu e o que ficou, e por quê. */
export function WatchedCard({ report }: { report: WatchedReport }) {
  return (
    <article className="grid gap-5 rounded-lg border border-border bg-surface p-5">
      {report.aviso && (
        <p className="flex items-start gap-2 rounded-md bg-warning-bg px-3 py-2 text-sm text-warning">
          <CircleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          {report.aviso}
        </p>
      )}

      <div className="grid gap-2">
        <h3 className="text-xs font-medium text-content-subtle">
          Apagados{report.apagados.length > 0 && ` — ${report.liberado} liberados`}
        </h3>
        {report.apagados.length === 0 ? (
          <p className="text-sm text-content-muted">Nenhum filme apagado nesta execução.</p>
        ) : (
          <ul className="divide-y divide-border rounded-md border border-border text-sm">
            {report.apagados.map((item) => (
              <li key={`${item.titulo}-${item.assistido_em}`} className="flex items-center justify-between gap-3 px-3 py-2">
                <span className="min-w-0 truncate" title={label(item.titulo, item.ano)}>
                  {label(item.titulo, item.ano)}
                </span>
                <span className="shrink-0 text-xs text-content-muted">
                  assistido por {item.assistido_por} em{' '}
                  <time dateTime={item.assistido_em}>{new Date(item.assistido_em).toLocaleString('pt-BR')}</time> ·{' '}
                  {item.tamanho}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>

      {report.pulados.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-medium text-content-subtle">Assistidos que ficaram, e por quê</h3>
          <ul className="grid gap-1 text-sm">
            {report.pulados.map((item) => (
              <li key={`${item.titulo}-${item.ano}`} className="flex flex-wrap gap-x-3">
                <span className="font-medium">{label(item.titulo, item.ano)}</span>
                <span className="text-content-muted">{item.motivo}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </article>
  )
}
