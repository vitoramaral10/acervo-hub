import {
  ArrowDownToLine,
  ArrowUpCircle,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  FilePen,
  Plus,
  Trash2,
  type LucideIcon,
} from 'lucide-react'
import type { HistoryEvent, HistoryEventKind } from '@/lib/api'
import { formatAgo } from '@/lib/format'
import { cn } from '@/lib/utils'

export const EVENTS: Record<HistoryEventKind, { label: string; icon: LucideIcon; tone: string }> = {
  grabbed: { label: 'Pegou', icon: ArrowDownToLine, tone: 'text-accent' },
  imported: { label: 'Importou', icon: CircleCheck, tone: 'text-success' },
  upgraded: { label: 'Trocou por versão melhor', icon: ArrowUpCircle, tone: 'text-success' },
  failed: { label: 'Falhou', icon: CircleAlert, tone: 'text-danger' },
  file_deleted: { label: 'Arquivo apagado', icon: Trash2, tone: 'text-content-muted' },
  movie_added: { label: 'Adicionado', icon: Plus, tone: 'text-content-muted' },
  movie_deleted: { label: 'Removido', icon: Trash2, tone: 'text-content-muted' },
  ignored: { label: 'Descartado', icon: CircleSlash, tone: 'text-content-muted' },
  renamed: { label: 'Renomeado', icon: FilePen, tone: 'text-content-muted' },
}

const dateTime = new Intl.DateTimeFormat('pt-BR', { dateStyle: 'short', timeStyle: 'short' })

function detail(event: HistoryEvent): string | null {
  const message = event.data?.mensagem
  if (typeof message === 'string') return message
  const reason = event.data?.message ?? event.data?.reason
  return typeof reason === 'string' ? reason : null
}

/** Eventos do histórico, do mais novo ao mais velho. `showMovie` põe o filme em cada linha. */
export function HistoryList({ events, showMovie }: { events: HistoryEvent[]; showMovie: boolean }) {
  return (
    <ol className="divide-y divide-border">
      {events.map((event) => {
        const kind = EVENTS[event.event] ?? EVENTS.ignored
        const Icon = kind.icon
        const note = detail(event)
        return (
          <li key={event.id} className="flex gap-3 py-3">
            <Icon className={cn('mt-0.5 size-4 shrink-0', kind.tone)} aria-hidden="true" />
            <div className="min-w-0 flex-1">
              <p className="text-sm">
                <span className="font-medium">{kind.label}</span>
                {showMovie && event.movie_title && <span className="text-content-muted"> · {event.movie_title}</span>}
              </p>
              {event.source_title && (
                <p className="truncate font-mono text-xs text-content-subtle" title={event.source_title}>
                  {event.source_title}
                </p>
              )}
              {note && (
                <p className={cn('mt-0.5 text-xs', event.event === 'failed' ? 'text-danger' : 'text-content-subtle')}>
                  {note}
                </p>
              )}
            </div>
            <div className="shrink-0 text-right text-xs text-content-subtle">
              <time dateTime={event.at} title={dateTime.format(new Date(event.at))}>
                {formatAgo(event.at)}
              </time>
              {(event.quality || event.indexer) && (
                <p className="mt-0.5">{[event.quality, event.indexer].filter(Boolean).join(' · ')}</p>
              )}
            </div>
          </li>
        )
      })}
    </ol>
  )
}
