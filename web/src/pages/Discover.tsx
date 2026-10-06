import { type InfiniteData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EyeOff, Plus, Star } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { AddMovieChoose } from '@/components/AddMovieChoose'
import { AddSeriesChoose } from '@/components/AddSeriesChoose'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  ApiError,
  discover,
  type DiscoverItem,
  type DiscoverSearchItem,
  type DiscoverList,
  type DiscoverPageResponse,
  type DiscoverReleases,
  type DiscoverWeek,
  type MediaKind,
  type SeriesTmdbResult,
  type TmdbResult,
} from '@/lib/api'
import { cn } from '@/lib/utils'
import { GRID, Poster } from '@/pages/Movies'

type Tab = 'lancamentos' | 'em_alta' | 'populares' | 'proximos' | 'ocultos'
const SERIES_STATUS: Record<string, string> = {
  'Returning Series': 'Em andamento',
  Ended: 'Encerrada',
  Canceled: 'Cancelada',
  'In Production': 'Em produção',
  Planned: 'Planejada',
  Pilot: 'Piloto',
}
const queryKey = ['descobrir'] as const
const itemKey = (item: Pick<DiscoverItem, 'tipo' | 'tmdb'>) => `${item.tipo}-${item.tmdb}`
const today = () => {
  const date = new Date()
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
}
const dateLabel = (date: string) =>
  new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit' }).format(new Date(`${date}T12:00:00`))
const weekLabel = (week: DiscoverWeek) =>
  `Semana ${week.semana} · ${dateLabel(week.inicio)} a ${dateLabel(week.fim)}${week.total == null ? '' : ` · ${week.total} títulos`}`

function ErrorState({ error, retry }: { error: Error; retry: () => void }) {
  return (
    <div role="alert" className="grid justify-items-start gap-3 rounded-md border border-border p-4">
      <p className="text-sm text-danger break-words">{error.message}</p>
      {error instanceof ApiError && error.status === 422 && /tmdb/i.test(error.message) && (
        <p className="text-sm text-content-muted">
          Configure a chave em{' '}
          <a href="#tmdb" className="text-accent underline">
            Configurações → TMDB
          </a>
          .
        </p>
      )}
      <Button size="sm" onClick={retry}>
        Tentar novamente
      </Button>
    </div>
  )
}

function LoadingGrid() {
  return (
    <div className={GRID} role="status" aria-label="Carregando títulos" aria-busy="true">
      {Array.from({ length: 8 }, (_, i) => (
        <div key={i} className="grid gap-2">
          <Skeleton className="aspect-[2/3]" />
          <Skeleton className="h-4" />
          <Skeleton className="h-3 w-2/3" />
        </div>
      ))}
    </div>
  )
}

function Empty({ children }: { children: string }) {
  return (
    <p className="py-10 text-center text-sm text-content-muted" role="status">
      {children}
    </p>
  )
}

/** Remove o mesmo título de todas as semanas e páginas já carregadas. */
function withoutTitle<T extends DiscoverReleases | DiscoverPageResponse>(data: T, item: DiscoverItem): T {
  return { ...data, itens: data.itens.filter((entry) => itemKey(entry) !== itemKey(item)) }
}

export function DiscoverPage() {
  const client = useQueryClient()
  const [tab, setTab] = useState<Tab>('lancamentos')
  const [kind, setKind] = useState<MediaKind>('filme')
  const [search, setSearch] = useState('')
  const [debounced, setDebounced] = useState('')
  const [detail, setDetail] = useState<Pick<DiscoverItem, 'tipo' | 'tmdb'> | null>(null)
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(search.trim()), 400)
    return () => window.clearTimeout(timer)
  }, [search])
  const [chosen, setChosen] = useState<DiscoverItem | null>(null)
  const hide = useMutation({
    mutationFn: (item: DiscoverItem) =>
      'oculto' in item && item.oculto
        ? discover.showTitle(item.tipo, item.tmdb)
        : discover.hideTitle({ tipo: item.tipo, tmdb: item.tmdb, titulo: item.titulo }),
    onMutate: async (item) => {
      await client.cancelQueries({ queryKey })
      const previous = client.getQueriesData({ queryKey })
      client.setQueriesData<DiscoverReleases>(
        { queryKey: [...queryKey, 'semana'] },
        (data) => data && withoutTitle(data, item),
      )
      client.setQueriesData<InfiniteData<DiscoverPageResponse>>(
        { queryKey: [...queryKey, 'lista'] },
        (data) => data && { ...data, pages: data.pages.map((page) => withoutTitle(page, item)) },
      )
      return previous
    },
    onError: (error, _item, previous) => {
      previous?.forEach(([key, data]) => client.setQueryData(key, data))
      toast.error(error.message)
    },
    onSuccess: (_data, item) =>
      toast.success('oculto' in item && item.oculto ? 'Título visível de novo' : 'Título ocultado'),
    onSettled: () => client.invalidateQueries({ queryKey }),
  })
  const added = () => {
    if (chosen) {
      // Retira da grade imediatamente enquanto o servidor recalcula os filtros.
      client.setQueriesData<DiscoverReleases>(
        { queryKey: [...queryKey, 'semana'] },
        (data) => data && withoutTitle(data, chosen),
      )
      client.setQueriesData<InfiniteData<DiscoverPageResponse>>(
        { queryKey: [...queryKey, 'lista'] },
        (data) => data && { ...data, pages: data.pages.map((page) => withoutTitle(page, chosen)) },
      )
    }
    setChosen(null)
    void client.invalidateQueries({ queryKey })
    void client.invalidateQueries({ queryKey: ['filmes'] })
    void client.invalidateQueries({ queryKey: ['series'] })
  }
  const cards = (items: DiscoverItem[], searching = false) =>
    items.length === 0 ? (
      <Empty>
        {searching
          ? 'Nenhum título encontrado.'
          : 'Nenhum título disponível. Títulos do acervo e ocultos ficam fora desta lista.'}
      </Empty>
    ) : (
      <div className={GRID}>
        {items.map((item) => (
          <article key={itemKey(item)} className="flex min-w-0 flex-col">
            <button
              type="button"
              onClick={() => setDetail(item)}
              aria-label={`Ver detalhes de ${item.titulo}`}
              className="rounded-md text-left focus-visible:outline-2 focus-visible:outline-ring"
            >
              <Poster movie={item} />
            </button>
            {searching && <Badge className="mt-2 self-start">{item.tipo === 'filme' ? 'Filme' : 'Série'}</Badge>}
            <h3 className="mt-2 line-clamp-2 text-sm leading-snug font-medium" title={item.titulo}>
              <button
                type="button"
                onClick={() => setDetail(item)}
                className="text-left hover:underline focus-visible:outline-2 focus-visible:outline-ring"
              >
                {item.titulo}
              </button>
            </h3>
            <p className="mt-1 flex flex-wrap items-center gap-x-2 text-xs text-content-subtle tabular-nums">
              <span>{item.ano ?? '—'}</span>
              <span className="inline-flex items-center gap-1">
                <Star className="size-3" aria-hidden="true" />
                {item.nota.toLocaleString('pt-BR', { maximumFractionDigits: 1 })}
              </span>
            </p>
            <p className="mt-1 text-xs text-content-muted break-words">{item.generos.join(' · ') || 'Sem gênero'}</p>
            <div className="mt-auto flex flex-wrap gap-1 pt-3">
              {'no_acervo' in item && item.no_acervo ? (
                <button
                  type="button"
                  className="rounded bg-accent-soft px-2 py-1 text-xs text-accent-soft-fg"
                  onClick={() => setDetail(item)}
                >
                  No acervo
                </button>
              ) : (
                <Button
                  size="sm"
                  className="min-w-0 px-2"
                  onClick={() => setChosen(item)}
                  aria-label={`Adicionar ${item.titulo}`}
                >
                  <Plus aria-hidden="true" />
                  Adicionar
                </Button>
              )}
              <Button
                size="sm"
                variant="ghost"
                className="min-w-0 px-2"
                disabled={hide.isPending}
                onClick={() => hide.mutate(item)}
                aria-label={`${'oculto' in item && item.oculto ? 'Mostrar de novo' : 'Ocultar'} ${item.titulo}`}
              >
                <EyeOff aria-hidden="true" />
                {'oculto' in item && item.oculto ? 'Mostrar de novo' : 'Ocultar'}
              </Button>
            </div>
          </article>
        ))}
      </div>
    )
  const result: TmdbResult | null = chosen
    ? {
        tmdb: chosen.tmdb,
        titulo: chosen.titulo,
        titulo_original: chosen.titulo_original,
        ano: chosen.ano,
        sinopse: chosen.sinopse,
        poster: chosen.poster,
        nota: chosen.nota,
        no_catalogo: null,
      }
    : null
  const seriesResult: SeriesTmdbResult | null = result
  return (
    <>
      <PageHeader title="Descobrir" description="Encontre lançamentos no Brasil e novos títulos para o seu acervo." />
      <div className="mb-6 grid gap-2">
        <Label htmlFor="descobrir-busca">Buscar filmes e séries</Label>
        <Input
          id="descobrir-busca"
          type="search"
          maxLength={100}
          placeholder="Título do filme ou série"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
      </div>
      {search.trim() ? (
        search.trim().length < 2 ? (
          <Empty>Digite pelo menos 2 caracteres para buscar.</Empty>
        ) : debounced !== search.trim() ? (
          <LoadingGrid />
        ) : (
          <SearchResults q={debounced} cards={(items) => cards(items, true)} />
        )
      ) : (
        <Tabs value={tab} onValueChange={(value) => setTab(value as Tab)}>
          <TabsList aria-label="Descobrir">
            <TabsTrigger value="lancamentos">Lançamentos</TabsTrigger>
            <TabsTrigger value="em_alta">Em alta</TabsTrigger>
            <TabsTrigger value="populares">Populares</TabsTrigger>
            <TabsTrigger value="proximos">{kind === 'filme' ? 'Em breve' : 'No ar'}</TabsTrigger>
            <TabsTrigger value="ocultos">Ocultos</TabsTrigger>
          </TabsList>
          <TabsContent value="lancamentos">
            <Releases cards={cards} />
          </TabsContent>
          {(['em_alta', 'populares', 'proximos'] as const).map((value) => (
            <TabsContent key={value} value={value}>
              <Lists
                list={value === 'proximos' ? (kind === 'filme' ? 'em_breve' : 'no_ar') : value}
                kind={kind}
                setKind={setKind}
                cards={cards}
              />
            </TabsContent>
          ))}
          <TabsContent value="ocultos">
            <Hidden />
          </TabsContent>
        </Tabs>
      )}
      <TitleDetails
        target={detail}
        onClose={() => setDetail(null)}
        onSelect={setDetail}
        onAdd={(item) => {
          setDetail(null)
          setChosen(item)
        }}
      />
      <Dialog
        open={chosen !== null}
        onOpenChange={(open) => {
          if (!open) setChosen(null)
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>Adicionar {chosen?.tipo === 'serie' ? 'série' : 'filme'}</DialogTitle>
            <DialogDescription>Escolha como adicionar este título ao acervo.</DialogDescription>
          </DialogHeader>
          {chosen?.tipo === 'filme' && result && (
            <AddMovieChoose
              key={itemKey(chosen)}
              result={result}
              onBack={() => setChosen(null)}
              onDone={added}
              backLabel="Cancelar"
            />
          )}
          {chosen?.tipo === 'serie' && seriesResult && (
            <AddSeriesChoose
              key={itemKey(chosen)}
              result={seriesResult}
              onBack={() => setChosen(null)}
              onDone={added}
              backLabel="Cancelar"
            />
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}

function SearchResults({ q, cards }: { q: string; cards: (items: DiscoverSearchItem[]) => ReactNode }) {
  const results = useInfiniteQuery({
    queryKey: [...queryKey, 'busca', q],
    initialPageParam: 1,
    queryFn: ({ pageParam }) => discover.search(q, pageParam),
    getNextPageParam: (page) => (page.pagina < Math.min(page.total_paginas, 500) ? page.pagina + 1 : undefined),
  })
  const items = [
    ...new Map(results.data?.pages.flatMap((page) => page.itens).map((item) => [itemKey(item), item])).values(),
  ]
  return (
    <section aria-label="Resultados da busca" className="space-y-5">
      {results.isPending ? (
        <LoadingGrid />
      ) : results.isError && !results.data ? (
        <ErrorState error={results.error} retry={() => void results.refetch()} />
      ) : (
        <>
          {cards(items)}
          {results.isError && (
            <ErrorState
              error={results.error}
              retry={() => void (results.isFetchNextPageError ? results.fetchNextPage() : results.refetch())}
            />
          )}
          {results.hasNextPage && (
            <div className="flex justify-center">
              <Button
                loading={results.isFetchingNextPage}
                disabled={results.isFetching}
                onClick={() => void results.fetchNextPage()}
              >
                Carregar mais
              </Button>
            </div>
          )}
        </>
      )}
    </section>
  )
}

function TitleDetails({
  target,
  onClose,
  onSelect,
  onAdd,
}: {
  target: Pick<DiscoverItem, 'tipo' | 'tmdb'> | null
  onClose: () => void
  onSelect: (item: DiscoverItem) => void
  onAdd: (item: DiscoverItem) => void
}) {
  const client = useQueryClient()
  const details = useQuery({
    queryKey: [...queryKey, 'titulo', target?.tipo, target?.tmdb],
    queryFn: () => discover.details(target!.tipo, target!.tmdb),
    enabled: !!target,
  })
  const change = useMutation({
    mutationFn: (item: DiscoverSearchItem) =>
      item.oculto ? discover.showTitle(item.tipo, item.tmdb) : discover.hideTitle(item),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey })
    },
    onError: (error) => toast.error(error.message),
  })
  const item = details.data
  return (
    <Dialog
      open={target !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent key={target ? itemKey(target) : 'fechado'} className="max-w-4xl" aria-describedby={undefined}>
        <DialogHeader>
          <DialogTitle>{item?.titulo ?? 'Detalhes do título'}</DialogTitle>
        </DialogHeader>
        {details.isPending ? (
          <div role="status" aria-label="Carregando detalhes" aria-busy="true">
            <Skeleton className="h-64" />
          </div>
        ) : details.isError ? (
          <ErrorState error={details.error} retry={() => void details.refetch()} />
        ) : (
          item && (
            <>
              {item.backdrop && (
                <img src={item.backdrop} alt="" className="aspect-video max-h-72 w-full rounded-md object-cover" />
              )}
              <div className="flex items-start gap-4">
                <div className="w-24 shrink-0 sm:w-36">
                  <Poster movie={item} />
                </div>
                <div className="min-w-0 space-y-2">
                  <p className="text-sm text-content-muted">
                    {item.tipo === 'filme' ? 'Filme' : 'Série'} · {item.ano ?? '—'} ·{' '}
                    {item.generos.join(' · ') || 'Sem gênero'}
                  </p>
                  <p className="text-sm text-content-muted">
                    {item.tipo === 'filme'
                      ? item.duracao
                        ? `${item.duracao} min`
                        : 'Duração indisponível'
                      : `${item.temporadas ?? '—'} temporadas · ${item.episodios ?? '—'} episódios`}
                  </p>
                  {item.tipo === 'serie' && item.status && (
                    <p className="text-sm text-content-muted">Status: {SERIES_STATUS[item.status] ?? item.status}</p>
                  )}
                  <p className="flex items-center gap-1 text-sm">
                    <Star className="size-4" aria-hidden="true" />
                    {item.nota.toLocaleString('pt-BR', { maximumFractionDigits: 1 })}
                  </p>
                  {item.tagline && <p className="text-sm italic text-content-muted">{item.tagline}</p>}
                  {item.diretores.length > 0 && <p className="text-sm">Direção: {item.diretores.join(', ')}</p>}
                  {item.criadores.length > 0 && <p className="text-sm">Criação: {item.criadores.join(', ')}</p>}
                </div>
              </div>
              <p className="text-sm leading-relaxed text-content-muted">{item.sinopse || 'Sem sinopse.'}</p>
              <div className="flex flex-wrap items-center gap-2">
                {item.no_acervo && item.id_acervo != null ? (
                  <a
                    href={`#${item.tipo === 'filme' ? 'filmes' : 'series'}?id=${item.id_acervo}`}
                    className="rounded bg-accent-soft px-3 py-2 text-sm text-accent-soft-fg"
                    onClick={onClose}
                  >
                    No acervo
                  </a>
                ) : (
                  <Button onClick={() => onAdd(item)}>
                    <Plus aria-hidden="true" />
                    Adicionar
                  </Button>
                )}
                <Button variant="ghost" loading={change.isPending} onClick={() => change.mutate(item)}>
                  {item.oculto ? 'Mostrar de novo' : 'Ocultar título'}
                </Button>
                {item.trailer && (
                  <Button asChild>
                    <a
                      href={`https://www.youtube.com/watch?v=${encodeURIComponent(item.trailer)}`}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      Ver trailer
                    </a>
                  </Button>
                )}
              </div>
              {item.elenco.length > 0 && (
                <section className="min-w-0 space-y-3" aria-label="Elenco">
                  <h3 className="font-semibold">Elenco</h3>
                  <div className="flex gap-3 overflow-x-auto pb-2">
                    {item.elenco.map((person, index) => (
                      <div key={`${person.nome}-${index}`} className="w-24 shrink-0 text-xs">
                        {person.foto ? (
                          <img
                            src={person.foto}
                            alt=""
                            loading="lazy"
                            className="aspect-[2/3] w-full rounded-md object-cover"
                          />
                        ) : (
                          <div className="aspect-[2/3] rounded-md bg-surface-raised" />
                        )}
                        <p className="mt-2 font-medium">{person.nome}</p>
                        <p className="text-content-subtle">{person.personagem}</p>
                      </div>
                    ))}
                  </div>
                </section>
              )}
              {item.recomendacoes.length > 0 && (
                <section className="min-w-0 space-y-3" aria-label="Recomendações">
                  <h3 className="font-semibold">Recomendações</h3>
                  <div className="flex gap-3 overflow-x-auto pb-2">
                    {item.recomendacoes.map((entry) => (
                      <button
                        key={itemKey(entry)}
                        type="button"
                        onClick={() => onSelect(entry)}
                        className="w-28 shrink-0 rounded-md text-left focus-visible:outline-2 focus-visible:outline-ring"
                      >
                        <Poster movie={entry} />
                        <span className="mt-2 block text-xs">{entry.titulo}</span>
                      </button>
                    ))}
                  </div>
                </section>
              )}
            </>
          )
        )}
      </DialogContent>
    </Dialog>
  )
}

type Cards = (items: DiscoverItem[]) => ReactNode

// Ano da semana ISO: o da quinta-feira da semana, como no servidor.
function isoWeekYear(date: Date) {
  const thursday = new Date(date)
  thursday.setDate(date.getDate() + 3 - ((date.getDay() + 6) % 7))
  return thursday.getFullYear()
}

function Releases({ cards }: { cards: Cards }) {
  const client = useQueryClient()
  const currentYear = isoWeekYear(new Date())
  const [year, setYear] = useState(currentYear)
  const [selected, setSelected] = useState<number | null>(null)
  const [order, setOrder] = useState('nota')
  const weeks = useQuery({ queryKey: [...queryKey, 'semanas', year], queryFn: () => discover.weeks(year) })
  const available = weeks.data?.semanas ?? []
  const week =
    available.find((entry) => entry.semana === selected) ??
    available.find((entry) => entry.inicio <= today() && entry.fim >= today()) ??
    available[0]
  const releases = useQuery({
    queryKey: [...queryKey, 'semana', year, week?.semana],
    queryFn: () => discover.releases(year, week!.semana),
    enabled: !!week,
  })
  const hideWeek = useMutation({
    mutationFn: () => discover.hideWeek(year, week!.semana),
    onSuccess: () => {
      setSelected(null)
      toast.success('Semana ocultada')
      void client.invalidateQueries({ queryKey })
    },
    onError: (error) => toast.error(error.message),
  })
  // A contagem retornada ao abrir uma semana já é filtrada, mesmo se o índice ainda não tinha cache.
  const labeledWeeks = available.map((entry) => ({
    ...entry,
    total:
      entry.semana === releases.data?.semana && releases.data.ano === year ? releases.data.itens.length : entry.total,
  }))
  return (
    <div className="grid gap-5 md:grid-cols-[14rem_minmax(0,1fr)]">
      <aside className="min-w-0 space-y-4">
        <div className="grid gap-2">
          <Label htmlFor="descobrir-ano">Ano</Label>
          <Select
            value={String(year)}
            onValueChange={(value) => {
              setYear(Number(value))
              setSelected(null)
            }}
          >
            <SelectTrigger id="descobrir-ano">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {Array.from({ length: 5 }, (_, i) => currentYear - 2 + i).map((value) => (
                <SelectItem key={value} value={String(value)}>
                  {value}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {weeks.isPending ? (
          <Skeleton className="h-32" />
        ) : weeks.isError ? (
          <ErrorState error={weeks.error} retry={() => void weeks.refetch()} />
        ) : available.length === 0 ? (
          <Empty>Nenhuma semana disponível neste ano.</Empty>
        ) : (
          <>
            <div className="grid gap-2 md:hidden">
              <Label htmlFor="descobrir-semana">Semana</Label>
              <Select value={String(week!.semana)} onValueChange={(value) => setSelected(Number(value))}>
                <SelectTrigger id="descobrir-semana" className="w-full min-w-0 [&>span]:truncate">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {labeledWeeks.map((entry) => (
                    <SelectItem key={entry.semana} value={String(entry.semana)}>
                      {weekLabel(entry)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <nav aria-label="Semanas de lançamentos" className="hidden max-h-[65vh] space-y-1 overflow-y-auto md:block">
              {labeledWeeks.map((entry) => (
                <button
                  key={entry.semana}
                  type="button"
                  aria-current={week?.semana === entry.semana ? 'true' : undefined}
                  onClick={() => setSelected(entry.semana)}
                  className={cn(
                    'grid w-full gap-1 rounded-md px-3 py-2 text-left text-sm hover:bg-surface-raised focus-visible:outline-2 focus-visible:outline-ring',
                    week?.semana === entry.semana && 'bg-accent-soft text-accent-soft-fg',
                  )}
                >
                  <span className="font-medium">Semana {entry.semana}</span>
                  <span className="text-xs">
                    {dateLabel(entry.inicio)} a {dateLabel(entry.fim)}
                    {entry.total != null && ` · ${entry.total} títulos`}
                  </span>
                </button>
              ))}
            </nav>
          </>
        )}
      </aside>
      <section className="min-w-0" aria-label="Lançamentos da semana">
        {week && (
          <>
            <div className="mb-5 flex flex-wrap items-end justify-between gap-3">
              <div>
                <h2 className="font-semibold">
                  Semana {week.semana} de {year}
                </h2>
                <p className="text-sm text-content-muted">
                  {dateLabel(week.inicio)} a {dateLabel(week.fim)}
                </p>
              </div>
              <div className="flex flex-wrap items-end gap-2">
                <div className="grid gap-1">
                  <Label htmlFor="descobrir-ordem">Ordenar por</Label>
                  <Select value={order} onValueChange={setOrder}>
                    <SelectTrigger id="descobrir-ordem">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="nota">Nota</SelectItem>
                      <SelectItem value="popularidade">Popularidade</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <Button size="sm" loading={hideWeek.isPending} onClick={() => hideWeek.mutate()}>
                  <EyeOff aria-hidden="true" />
                  Ocultar semana
                </Button>
              </div>
            </div>
            {releases.isPending ? (
              <LoadingGrid />
            ) : releases.isError ? (
              <ErrorState error={releases.error} retry={() => void releases.refetch()} />
            ) : (
              cards(
                [...releases.data.itens].sort((a, b) =>
                  order === 'nota' ? b.nota - a.nota : b.popularidade - a.popularidade,
                ),
              )
            )}
          </>
        )}
      </section>
    </div>
  )
}

function Lists({
  list,
  kind,
  setKind,
  cards,
}: {
  list: DiscoverList
  kind: MediaKind
  setKind: (kind: MediaKind) => void
  cards: Cards
}) {
  const results = useInfiniteQuery({
    queryKey: [...queryKey, 'lista', list, kind],
    initialPageParam: 1,
    queryFn: ({ pageParam }) => discover.list(list, kind, pageParam),
    getNextPageParam: (page) => (page.pagina < Math.min(page.total_paginas, 500) ? page.pagina + 1 : undefined),
  })
  const items = [
    ...new Map(results.data?.pages.flatMap((page) => page.itens).map((item) => [itemKey(item), item])).values(),
  ]
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap gap-2" role="group" aria-label="Tipo de título">
        <Button
          size="sm"
          variant={kind === 'filme' ? 'primary' : 'secondary'}
          aria-pressed={kind === 'filme'}
          onClick={() => setKind('filme')}
        >
          Filmes
        </Button>
        <Button
          size="sm"
          variant={kind === 'serie' ? 'primary' : 'secondary'}
          aria-pressed={kind === 'serie'}
          onClick={() => setKind('serie')}
        >
          Séries
        </Button>
      </div>
      {results.isPending ? (
        <LoadingGrid />
      ) : results.isError && !results.data ? (
        <ErrorState error={results.error} retry={() => void results.refetch()} />
      ) : (
        <>
          {cards(items)}
          {results.isError && (
            <ErrorState
              error={results.error}
              retry={() => void (results.isFetchNextPageError ? results.fetchNextPage() : results.refetch())}
            />
          )}
          {results.hasNextPage && (
            <div className="flex justify-center">
              <Button
                loading={results.isFetchingNextPage}
                disabled={results.isFetching}
                onClick={() => void results.fetchNextPage()}
              >
                Carregar mais
              </Button>
            </div>
          )}
        </>
      )}
    </div>
  )
}

function Hidden() {
  const client = useQueryClient()
  const hidden = useQuery({ queryKey: [...queryKey, 'ocultos'], queryFn: discover.hidden })
  const genres = useQuery({ queryKey: [...queryKey, 'generos'], queryFn: discover.genres })
  const change = useMutation({
    mutationFn: (action: () => Promise<unknown>) => action(),
    onSuccess: () => {
      toast.success('Preferências atualizadas')
      void client.invalidateQueries({ queryKey })
    },
    onError: (error) => toast.error(error.message),
  })
  const show = (action: () => Promise<unknown>, title: string) => (
    <Button
      size="sm"
      disabled={change.isPending}
      onClick={() => change.mutate(action)}
      aria-label={`Mostrar de novo: ${title}`}
    >
      Mostrar de novo
    </Button>
  )
  const rowClass = 'flex min-w-0 flex-wrap items-center justify-between gap-3 rounded-md border border-border p-3'
  return (
    <div className="space-y-8">
      {hidden.isPending ? (
        <div className="grid gap-3" role="status" aria-label="Carregando ocultos">
          <Skeleton className="h-20" />
          <Skeleton className="h-20" />
          <Skeleton className="h-20" />
        </div>
      ) : hidden.isError ? (
        <ErrorState error={hidden.error} retry={() => void hidden.refetch()} />
      ) : (
        <>
          <section className="space-y-3">
            <h2 className="font-semibold">Títulos ocultos</h2>
            {hidden.data.titulos.length === 0 ? (
              <Empty>Nenhum título oculto.</Empty>
            ) : (
              <ul className="space-y-2">
                {hidden.data.titulos.map((entry) => (
                  <li className={rowClass} key={itemKey(entry)}>
                    <p className="min-w-0 flex-1 break-words text-sm">
                      {entry.titulo || `TMDB ${entry.tmdb}`}{' '}
                      <span className="text-content-subtle">· {entry.tipo === 'filme' ? 'Filme' : 'Série'}</span>
                    </p>
                    {show(() => discover.showTitle(entry.tipo, entry.tmdb), entry.titulo)}
                  </li>
                ))}
              </ul>
            )}
          </section>
          <section className="space-y-3">
            <h2 className="font-semibold">Semanas ocultas</h2>
            {hidden.data.semanas.length === 0 ? (
              <Empty>Nenhuma semana oculta.</Empty>
            ) : (
              <ul className="space-y-2">
                {hidden.data.semanas.map((entry) => (
                  <li className={rowClass} key={`${entry.ano}-${entry.semana}`}>
                    <p className="text-sm">
                      Semana {entry.semana} de {entry.ano}
                    </p>
                    {show(() => discover.showWeek(entry.ano, entry.semana), `semana ${entry.semana} de ${entry.ano}`)}
                  </li>
                ))}
              </ul>
            )}
          </section>
          <section className="space-y-3">
            <h2 className="font-semibold">Gêneros ocultos</h2>
            {hidden.data.generos.length === 0 ? (
              <Empty>Nenhum gênero oculto.</Empty>
            ) : (
              <ul className="space-y-2">
                {hidden.data.generos.map((entry) => (
                  <li className={rowClass} key={entry.id}>
                    <p className="min-w-0 flex-1 break-words text-sm">{entry.nome || `Gênero ${entry.id}`}</p>
                    {show(() => discover.showGenre(entry.id), entry.nome)}
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}
      <section className="space-y-3">
        <h2 className="font-semibold">Todos os gêneros</h2>
        <p className="text-sm text-content-muted">
          Ative para ocultar todos os títulos desse gênero nas listas do Descobrir.
        </p>
        {genres.isPending ? (
          <Skeleton className="h-32" />
        ) : genres.isError ? (
          <ErrorState error={genres.error} retry={() => void genres.refetch()} />
        ) : genres.data.generos.length === 0 ? (
          <Empty>Nenhum gênero disponível.</Empty>
        ) : (
          <ul className="grid gap-2 sm:grid-cols-2">
            {genres.data.generos.map((genre) => (
              <li className={rowClass} key={genre.id}>
                <Label htmlFor={`ocultar-genero-${genre.id}`} className="min-w-0 flex-1 break-words">
                  {genre.nome}
                </Label>
                <Switch
                  id={`ocultar-genero-${genre.id}`}
                  checked={genre.oculto}
                  disabled={change.isPending}
                  aria-label={`Ocultar ${genre.nome}`}
                  onCheckedChange={(checked) =>
                    change.mutate(() =>
                      checked ? discover.hideGenre(genre.id, genre.nome) : discover.showGenre(genre.id),
                    )
                  }
                />
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}
