import { type InfiniteData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EyeOff, Plus, Star } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { toast } from 'sonner'
import { AddTitleDialog } from '@/components/AddTitleDialog'
import { TitleDetails } from '@/components/TitleDetails'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  ApiError,
  discover,
  type DiscoverItem,
  type DiscoverList,
  type DiscoverPageResponse,
  type DiscoverReleases,
  type MediaKind,
} from '@/lib/api'
import { GRID, Poster } from '@/pages/Movies'

type Tab = 'lancamentos' | 'em_alta' | 'populares' | 'proximos' | 'ocultos'
const queryKey = ['descobrir'] as const
const itemKey = (item: Pick<DiscoverItem, 'tipo' | 'tmdb'>) => `${item.tipo}-${item.tmdb}`
const dateLabel = (date: string) =>
  new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit' }).format(new Date(`${date}T12:00:00`))

function ErrorState({ error, retry }: { error: Error; retry: () => void }) {
  return (
    <div role="alert" className="grid justify-items-start gap-3 rounded-md border border-border p-4">
      <p className="text-sm text-danger break-words">{error.message}</p>
      {error instanceof ApiError && error.status === 422 && /tmdb/i.test(error.message) && (
        <p className="text-sm text-content-muted">
          Configure a chave em{' '}
          <a href="#configuracoes?secao=tmdb" className="text-accent underline">
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
  const [detail, setDetail] = useState<Pick<DiscoverItem, 'tipo' | 'tmdb'> | null>(null)
  const [chosen, setChosen] = useState<DiscoverItem | null>(null)
  const hide = useMutation({
    mutationFn: (item: DiscoverItem) =>
      'oculto' in item && item.oculto
        ? discover.showTitle(item.tipo, item.tmdb)
        : discover.hideTitle({ tipo: item.tipo, tmdb: item.tmdb, titulo: item.titulo }),
    onMutate: async (item) => {
      await client.cancelQueries({ queryKey })
      const previous = client.getQueriesData({ queryKey })
      client.setQueriesData<InfiniteData<ReleasesPage>>(
        { queryKey: [...queryKey, 'lancamentos'] },
        (data) => data && {
          ...data,
          pages: data.pages.map((page) => ({
            ...page,
            weeks: page.weeks.map((week) => withoutTitle(week, item)),
          })),
        },
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
      client.setQueriesData<InfiniteData<ReleasesPage>>(
        { queryKey: [...queryKey, 'lancamentos'] },
        (data) => data && {
          ...data,
          pages: data.pages.map((page) => ({
            ...page,
            weeks: page.weeks.map((week) => withoutTitle(week, chosen)),
          })),
        },
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
  const cards = (items: DiscoverItem[]) =>
    items.length === 0 ? (
      <Empty>Nenhum título disponível. Títulos do acervo e ocultos ficam fora desta lista.</Empty>
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
  return (
    <>
      <PageHeader title="Descobrir" description="Encontre lançamentos no Brasil e novos títulos para o seu acervo." />
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
      <TitleDetails
        target={detail}
        onClose={() => setDetail(null)}
        onSelect={setDetail}
        onAdd={(item) => {
          setDetail(null)
          setChosen(item)
        }}
      />
      <AddTitleDialog chosen={chosen} onClose={() => setChosen(null)} onDone={added} />
    </>
  )
}

type Cards = (items: DiscoverItem[]) => ReactNode

// Ano da semana ISO: o da quinta-feira da semana, como no servidor.
function isoWeekYear(date: Date) {
  const thursday = new Date(date)
  thursday.setDate(date.getDate() + 3 - ((date.getDay() + 6) % 7))
  return thursday.getFullYear()
}

type IsoWeek = { year: number; week: number }
type ReleasesPage = { weeks: DiscoverReleases[]; next: IsoWeek | undefined }
const WEEK_MS = 7 * 24 * 60 * 60 * 1000

// Aritmética UTC evita que o horário de verão altere a distância entre segundas ISO.
function weekStart({ year, week }: IsoWeek) {
  const january4 = new Date(Date.UTC(year, 0, 4))
  return january4.getTime() - ((january4.getUTCDay() + 6) % 7) * (WEEK_MS / 7) + (week - 1) * WEEK_MS
}

function isoWeek(date: Date): IsoWeek {
  const year = isoWeekYear(date)
  const day = new Date(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()))
  const monday = day.getTime() - ((day.getUTCDay() + 6) % 7) * (WEEK_MS / 7)
  return { year, week: 1 + Math.round((monday - weekStart({ year, week: 1 })) / WEEK_MS) }
}

function previousWeek(current: IsoWeek): IsoWeek {
  if (current.week > 1) return { ...current, week: current.week - 1 }
  // 28 de dezembro sempre pertence à última semana ISO (52 ou 53).
  return isoWeek(new Date(current.year - 1, 11, 28))
}

function Releases({ cards }: { cards: Cards }) {
  const [range] = useState(() => {
    const now = new Date()
    const oldest = new Date(now)
    oldest.setFullYear(now.getFullYear() - 2)
    return { start: isoWeek(now), oldest: weekStart(isoWeek(oldest)) }
  })
  const [order, setOrder] = useState('nota')
  const releases = useInfiniteQuery({
    queryKey: [...queryKey, 'lancamentos', range.start],
    initialPageParam: range.start,
    queryFn: async ({ pageParam, signal }): Promise<ReleasesPage> => {
      let next: IsoWeek | undefined = pageParam
      const weeks: DiscoverReleases[] = []
      for (let scanned = 0; scanned < 8 && next; scanned++) {
        signal.throwIfAborted()
        const week = await discover.releases(next.year, next.week)
        signal.throwIfAborted()
        const previous = previousWeek(next)
        next = weekStart(previous) >= range.oldest ? previous : undefined
        if (week.itens.length > 0) {
          weeks.push(week)
          break
        }
      }
      return { weeks, next }
    },
    getNextPageParam: (page) => page.next,
  })
  const weeks = releases.data?.pages.flatMap((page) => page.weeks).filter((week) => week.itens.length > 0) ?? []
  return (
    <div className="space-y-5">
      <div className="grid w-fit gap-1">
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
      {releases.isPending ? (
        <LoadingGrid />
      ) : releases.isError && !releases.data ? (
        <ErrorState error={releases.error} retry={() => void releases.refetch()} />
      ) : (
        <>
          {weeks.length === 0 ? (
            <Empty>Nenhum título disponível. Títulos do acervo e ocultos ficam fora desta lista.</Empty>
          ) : (
            weeks.map((week) => (
              <section
                key={`${week.ano}-${week.semana}`}
                className="space-y-3"
                aria-labelledby={`semana-${week.ano}-${week.semana}`}
              >
                <h2 id={`semana-${week.ano}-${week.semana}`} className="text-sm font-semibold text-content-muted">
                  Semana de {dateLabel(week.inicio)} a {dateLabel(week.fim)}
                </h2>
                {cards(
                  [...week.itens].sort((a, b) =>
                    order === 'nota' ? b.nota - a.nota : b.popularidade - a.popularidade,
                  ),
                )}
              </section>
            ))
          )}
          {releases.isError && (
            <ErrorState
              error={releases.error}
              retry={() => void (releases.isFetchNextPageError ? releases.fetchNextPage() : releases.refetch())}
            />
          )}
          {releases.hasNextPage && (
            <div className="flex justify-center">
              <Button
                loading={releases.isFetchingNextPage}
                disabled={releases.isFetching}
                onClick={() => void releases.fetchNextPage()}
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
        </>
      )}
    </div>
  )
}
