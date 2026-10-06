import { useQuery } from '@tanstack/react-query'
import { LoaderCircle, SearchX, Star, Tv } from 'lucide-react'
import { useMemo, useState } from 'react'
import { MarkedBadge, useMarkedKeys } from '@/components/DeletionMark'
import { PageHeader } from '@/components/PageHeader'
import { PriorityBadge } from '@/components/PriorityStar'
import { SeriesDetails } from '@/components/SeriesDetails'
import { SeriesPoster } from '@/components/SeriesPoster'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type SeriesSummary, seriesApi } from '@/lib/api'
import { formatCount, formatSize } from '@/lib/format'
import {
  SERIES_FILTERS,
  type SeriesFilter,
  matchesFilter,
  matchesQuery,
  seriesTitleKey,
} from '@/lib/seriesFormat'
import { cn } from '@/lib/utils'
import { Chip, ChipGroup, GRID } from '@/pages/Movies'

/** O que falta dizer sobre a série na capa; série completa não precisa de marca. */
function PosterMarks({ series }: { series: SeriesSummary }) {
  const { quero_exibidos: missing, baixando: downloading } = series.episodios
  if (missing === 0 && downloading === 0) return null
  return (
    <div className="absolute top-2 left-2 flex max-w-[calc(100%-1rem)] flex-col items-start gap-1">
      {missing > 0 && (
        <Badge tone="warning" className="shadow-sm">
          {missing} {missing === 1 ? 'falta' : 'faltam'}
        </Badge>
      )}
      {downloading > 0 && (
        <Badge tone="accent" className="shadow-sm">
          <LoaderCircle className="animate-spin motion-reduce:animate-none" aria-hidden="true" />
          {downloading} baixando
        </Badge>
      )}
    </div>
  )
}

function PosterCard({ series, marked, onOpen }: { series: SeriesSummary; marked: boolean; onOpen: () => void }) {
  const { tenho } = series.episodios
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group block w-full rounded-md text-left focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring"
    >
      <div className="relative">
        <SeriesPoster
          poster={series.poster}
          title={series.titulo}
          className={cn(
            'transition-[transform,opacity] duration-150 ease-out group-hover:-translate-y-0.5 group-hover:border-border-strong motion-reduce:transform-none',
            tenho === 0 && 'opacity-60 group-hover:opacity-100',
          )}
        />
        <PosterMarks series={series} />
        {series.prioritario && <PriorityBadge className="absolute top-2 right-2" />}
        {marked && <MarkedBadge className="absolute bottom-2 left-2" />}
      </div>
      <p className="mt-2 line-clamp-2 text-sm leading-snug font-medium">{series.titulo}</p>
      <p className="mt-0.5 text-xs text-content-subtle tabular-nums">
        {series.ano ?? '—'} · {tenho} {tenho === 1 ? 'episódio' : 'episódios'}
      </p>
    </button>
  )
}

function LibrarySkeleton() {
  return (
    <div aria-busy="true" aria-label="Carregando séries">
      <Skeleton className="mb-6 h-9 w-80 max-w-full" />
      <div className={GRID}>
        {Array.from({ length: 18 }, (_, key) => (
          <div key={key}>
            <Skeleton className="aspect-[2/3] rounded-md" />
            <Skeleton className="mt-2 h-4 w-3/4" />
            <Skeleton className="mt-1 h-3 w-1/3" />
          </div>
        ))}
      </div>
    </div>
  )
}

export function SeriesPage() {
  const series = useQuery({
    queryKey: ['series'],
    queryFn: seriesApi.list,
    // Enquanto algum episódio baixa, a lista acompanha o que o servidor importa.
    refetchInterval: (state) =>
      state.state.data?.series.some((item) => item.episodios.baixando > 0) ? 5_000 : false,
  })
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<SeriesFilter>('todas')
  const [selected, setSelected] = useState<number | null>(() => {
    const value = new URLSearchParams(window.location.hash.split('?')[1]).get('id')
    return value && /^\d+$/.test(value) && Number(value) > 0 ? Number(value) : null
  })
  // Série com alguma marca (inteira ou de temporada): `serie-<id>` ou `serie-<id>-t<n>`.
  const marks = useMarkedKeys()
  const markedSeries = useMemo(() => {
    const ids = new Set<number>()
    for (const key of marks) {
      const match = /^serie-(\d+)/.exec(key)
      if (match) ids.add(Number(match[1]))
    }
    return ids
  }, [marks])
  const [priority, setPriority] = useState(false)

  const list = series.data?.series
  const counts = useMemo(() => {
    const all = list ?? []
    return {
      total: all.length,
      withFile: all.reduce((sum, s) => sum + s.episodios.tenho, 0),
      size: all.reduce((sum, s) => sum + s.tamanho, 0),
    }
  }, [list])

  const searched = useMemo(() => (list ?? []).filter((s) => matchesQuery(s, query)), [list, query])
  const filterTotals = useMemo(
    () => Object.fromEntries(SERIES_FILTERS.map(({ value }) => [value, searched.filter((s) => matchesFilter(s, value)).length])),
    [searched],
  )
  const visible = useMemo(
    () =>
      searched
        .filter((s) => matchesFilter(s, filter) && (!priority || s.prioritario))
        .sort((a, b) => seriesTitleKey(a).localeCompare(seriesTitleKey(b), 'pt-BR')),
    [searched, filter, priority],
  )

  return (
    <>
      <PageHeader
        title="Séries"
        description={
          series.isSuccess
            ? `${formatCount(counts.total)} séries, ${formatCount(counts.withFile)} episódios no disco (${formatSize(counts.size)}).`
            : 'Sua biblioteca de séries.'
        }
      />

      {series.isPending ? (
        <LibrarySkeleton />
      ) : series.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível ler o catálogo.</p>
          <p className="text-sm text-content-muted">{series.error.message}</p>
          <Button onClick={() => void series.refetch()}>Tentar novamente</Button>
        </div>
      ) : counts.total === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <Tv className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">A biblioteca de séries está vazia</p>
          <p className="max-w-md text-sm text-content-muted">Use “Buscar títulos” no menu, / ou Ctrl/Cmd+K para adicionar séries.</p>
        </div>
      ) : (
        <>
          <div className="mb-5 flex flex-wrap items-center gap-2">
            <label className="min-w-0 flex-1 basis-56 sm:max-w-xs sm:flex-none lg:w-64">
              <span className="sr-only">Buscar na biblioteca</span>
              <Input
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Buscar por título, ano ou rede"
                className="h-8"
              />
            </label>
            <div className="min-w-0 overflow-x-auto">
              <ChipGroup label="Estado" single nowrap>
                {SERIES_FILTERS.map(({ value, label }) => (
                  <Chip key={value} single active={filter === value} onClick={() => setFilter(value)}>
                    {label}
                    <span className="text-content-subtle tabular-nums">{formatCount(filterTotals[value])}</span>
                  </Chip>
                ))}
              </ChipGroup>
            </div>
            <ChipGroup label="Prioridade">
              <Chip active={priority} onClick={() => setPriority((on) => !on)}>
                <Star className={cn('size-3.5', priority && 'fill-warning text-warning')} aria-hidden="true" />
                Prioritários
              </Chip>
            </ChipGroup>
          </div>

          {visible.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
              <SearchX className="size-6 text-content-subtle" aria-hidden="true" />
              <p className="font-medium">Nenhuma série com esse filtro</p>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setQuery('')
                  setFilter('todas')
                  setPriority(false)
                }}
              >
                Limpar filtros
              </Button>
            </div>
          ) : (
            <ul className={GRID} aria-label="Séries">
              {visible.map((item) => (
                <li key={item.id}>
                  <PosterCard series={item} marked={markedSeries.has(item.id)} onOpen={() => setSelected(item.id)} />
                </li>
              ))}
            </ul>
          )}
          {visible.length !== counts.total && (
            <p className="mt-4 text-xs text-content-subtle tabular-nums">
              {formatCount(visible.length)} de {formatCount(counts.total)}
            </p>
          )}
        </>
      )}

      {selected !== null && <SeriesDetails id={selected} onClose={() => setSelected(null)} />}
    </>
  )
}
