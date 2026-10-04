import { useQuery } from '@tanstack/react-query'
import { CalendarX, ChevronLeft, ChevronRight, LoaderCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { ItemDetails, type ItemRef } from '@/components/ItemDetails'
import { MediaThumb } from '@/components/MediaThumb'
import { PageHeader } from '@/components/PageHeader'
import { PriorityBadge } from '@/components/PriorityStar'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type CalendarItem, type ReleaseKind, library } from '@/lib/api'
import { episodeCode } from '@/lib/seriesFormat'
import { cn } from '@/lib/utils'
import { Chip, ChipGroup } from '@/pages/Movies'

type Span = 'semana' | 'mes'

const pad = (n: number) => String(n).padStart(2, '0')
/** `AAAA-MM-DD` no fuso local: o dia que o usuário vê, não o do UTC. */
const iso = (date: Date) => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
const fromIso = (day: string) => {
  const [year = 0, month = 1, date = 1] = day.split('-').map(Number)
  return new Date(year, month - 1, date)
}

/** Segunda a domingo, ou o mês inteiro, em volta de `anchor`. */
function rangeOf(anchor: Date, span: Span): { de: string; ate: string } {
  if (span === 'mes') {
    return {
      de: iso(new Date(anchor.getFullYear(), anchor.getMonth(), 1)),
      ate: iso(new Date(anchor.getFullYear(), anchor.getMonth() + 1, 0)),
    }
  }
  const monday = new Date(anchor.getFullYear(), anchor.getMonth(), anchor.getDate() - ((anchor.getDay() + 6) % 7))
  return { de: iso(monday), ate: iso(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + 6)) }
}

function shift(anchor: Date, span: Span, direction: -1 | 1): Date {
  return span === 'mes'
    ? new Date(anchor.getFullYear(), anchor.getMonth() + direction, 1)
    : new Date(anchor.getFullYear(), anchor.getMonth(), anchor.getDate() + 7 * direction)
}

const dayFormat = new Intl.DateTimeFormat('pt-BR', { weekday: 'long', day: 'numeric', month: 'long' })
const shortFormat = new Intl.DateTimeFormat('pt-BR', { day: 'numeric', month: 'short' })
const monthFormat = new Intl.DateTimeFormat('pt-BR', { month: 'long', year: 'numeric' })

function rangeLabel(de: string, ate: string, span: Span) {
  return span === 'mes' ? monthFormat.format(fromIso(de)) : `${shortFormat.format(fromIso(de))} a ${shortFormat.format(fromIso(ate))}`
}

const RELEASE: Record<ReleaseKind, string> = { cinema: 'Cinema', digital: 'Digital', fisico: 'Físico' }

function StateBadge({ item }: { item: CalendarItem }) {
  if (item.tipo === 'filme') {
    if (item.estado === 'tenho') return <Badge tone="success">No disco</Badge>
    if (item.estado === 'baixando') return <Downloading />
    return <Badge tone="warning">Falta</Badge>
  }
  if (item.estado === 'tenho') return <Badge tone="success">No disco</Badge>
  if (item.estado === 'dispensado') return <Badge>Dispensado</Badge>
  if (item.baixando) return <Downloading />
  return item.exibido ? <Badge tone="warning">Falta</Badge> : <Badge>Aguardando</Badge>
}

const Downloading = () => (
  <Badge tone="accent">
    <LoaderCircle className="animate-spin motion-reduce:animate-none" aria-hidden="true" />
    Baixando
  </Badge>
)

function CalendarRow({ item, onOpen }: { item: CalendarItem; onOpen: () => void }) {
  const episode = item.tipo === 'episodio'
  return (
    <button
      type="button"
      onClick={onOpen}
      className="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors hover:bg-surface-raised focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
    >
      <MediaThumb poster={item.poster} kind={episode ? 'serie' : 'filme'} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{episode ? item.serie : item.filme}</p>
        <p className="truncate text-xs text-content-muted">
          {episode
            ? `${episodeCode(item.temporada, item.numero)}${item.titulo ? ` · ${item.titulo}` : ''}`
            : `${item.ano ? `${item.ano} · ` : ''}Lançamento ${RELEASE[item.lancamento].toLowerCase()}`}
        </p>
      </div>
      <div className="flex shrink-0 flex-wrap items-center justify-end gap-1.5">
        {item.prioritario && <PriorityBadge />}
        {!episode && <Badge tone="neutral">{RELEASE[item.lancamento]}</Badge>}
        <StateBadge item={item} />
      </div>
    </button>
  )
}

export function CalendarPage() {
  const [span, setSpan] = useState<Span>('mes')
  const [anchor, setAnchor] = useState(() => new Date())
  const [open, setOpen] = useState<ItemRef | null>(null)
  const { de, ate } = useMemo(() => rangeOf(anchor, span), [anchor, span])
  const today = iso(new Date())
  const calendar = useQuery({ queryKey: ['calendario', de, ate], queryFn: () => library.calendar(de, ate) })

  // Só os dias com algo; hoje aparece sempre que cai no período, para ancorar a leitura.
  const days = useMemo(() => {
    const byDay = new Map<string, CalendarItem[]>()
    for (const item of calendar.data?.itens ?? []) {
      const day = item.data.slice(0, 10)
      byDay.set(day, [...(byDay.get(day) ?? []), item])
    }
    if (today >= de && today <= ate && !byDay.has(today)) byDay.set(today, [])
    return [...byDay.entries()].sort(([a], [b]) => a.localeCompare(b))
  }, [calendar.data, today, de, ate])

  return (
    <>
      <PageHeader
        title="Calendário"
        description="Episódios e lançamentos de filmes por dia, com o que já está no disco e o que falta."
      />
      <div className="mb-5 flex flex-wrap items-center gap-2">
        <div className="flex items-center gap-1">
          <Button variant="secondary" size="icon-sm" aria-label={span === 'mes' ? 'Mês anterior' : 'Semana anterior'} onClick={() => setAnchor(shift(anchor, span, -1))}>
            <ChevronLeft aria-hidden="true" />
          </Button>
          <Button variant="secondary" size="icon-sm" aria-label={span === 'mes' ? 'Próximo mês' : 'Próxima semana'} onClick={() => setAnchor(shift(anchor, span, 1))}>
            <ChevronRight aria-hidden="true" />
          </Button>
          <Button variant="secondary" size="sm" onClick={() => setAnchor(new Date())}>
            Hoje
          </Button>
        </div>
        <p className="min-w-0 flex-1 text-sm font-medium capitalize" aria-live="polite">
          {rangeLabel(de, ate, span)}
        </p>
        <ChipGroup label="Período" single>
          <Chip single active={span === 'semana'} onClick={() => setSpan('semana')}>
            Semana
          </Chip>
          <Chip single active={span === 'mes'} onClick={() => setSpan('mes')}>
            Mês
          </Chip>
        </ChipGroup>
      </div>

      {calendar.isPending ? (
        <div className="grid gap-2" aria-busy="true" aria-label="Carregando o calendário">
          {Array.from({ length: 6 }, (_, key) => (
            <Skeleton key={key} className="h-16" />
          ))}
        </div>
      ) : calendar.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível ler o calendário.</p>
          <p className="text-sm text-content-muted">{calendar.error.message}</p>
          <Button onClick={() => void calendar.refetch()}>Tentar novamente</Button>
        </div>
      ) : days.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <CalendarX className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nada neste período</p>
          <p className="max-w-md text-sm text-content-muted">Nenhum episódio nem lançamento de filme entre as datas.</p>
        </div>
      ) : (
        <div className="grid gap-5">
          {days.map(([day, items]) => {
            const isToday = day === today
            return (
              <section key={day} aria-labelledby={`dia-${day}`} aria-current={isToday ? 'date' : undefined}>
                <h2
                  id={`dia-${day}`}
                  className={cn(
                    'mb-1.5 flex items-center gap-2 text-sm font-semibold capitalize',
                    isToday ? 'text-accent' : 'text-content-muted',
                  )}
                >
                  {dayFormat.format(fromIso(day))}
                  {isToday && <Badge tone="accent">Hoje</Badge>}
                </h2>
                {items.length === 0 ? (
                  <p className="rounded-lg border border-dashed border-border-strong px-3 py-3 text-sm text-content-subtle">
                    Nada hoje.
                  </p>
                ) : (
                  <ul
                    className={cn(
                      'divide-y divide-border overflow-hidden rounded-lg border bg-surface',
                      isToday ? 'border-accent' : 'border-border',
                    )}
                  >
                    {items.map((item) => (
                      <li key={item.tipo === 'episodio' ? `e${item.episodio_id}` : `f${item.filme_id}${item.lancamento}`}>
                        <CalendarRow
                          item={item}
                          onOpen={() =>
                            setOpen(
                              item.tipo === 'episodio'
                                ? { kind: 'serie', id: item.serie_id }
                                : { kind: 'filme', id: item.filme_id },
                            )
                          }
                        />
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            )
          })}
        </div>
      )}

      {open && <ItemDetails item={open} onClose={() => setOpen(null)} />}
    </>
  )
}
