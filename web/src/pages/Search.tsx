import { useQuery } from '@tanstack/react-query'
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  CircleAlert,
  Loader2,
  Download,
  ExternalLink,
  Magnet,
  Search as SearchIcon,
  SearchX,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Tooltip } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type Release, api } from '@/lib/api'
import { ageInSeconds, formatAgo, formatCount, formatSize, safeHref } from '@/lib/format'
import { cn } from '@/lib/utils'

const ALL = 'todos'

const CATEGORIES = [
  { value: ALL, label: 'Todas' },
  { value: '2000', label: 'Filmes' },
  { value: '5000', label: 'Séries' },
  { value: '5070', label: 'Anime' },
  { value: '7000', label: 'Livros' },
  { value: '3000', label: 'Áudio' },
  { value: '8000', label: 'Outros' },
]

/** Nome curto da categoria Newznab, pela faixa. */
function categoryLabel(ids: number[]): string | null {
  // Anime prevalece: vem quase sempre junto de uma categoria genérica.
  if (ids.includes(5070)) return 'Anime'
  const id = ids[0]
  if (id === undefined) return null
  const group = Math.floor(id / 1000) * 1000
  return (
    { 1000: 'Console', 2000: 'Filme', 3000: 'Áudio', 4000: 'PC', 5000: 'Série', 6000: 'Adulto', 7000: 'Livro', 8000: 'Outro' }[
      group
    ] ?? null
  )
}

type SortKey = 'relevancia' | 'tamanho' | 'seeders' | 'idade'

interface Params {
  q: string
  indexador: string
  cat: string
}

export function SearchPage() {
  const indexers = useQuery({ queryKey: ['indexadores'], queryFn: api.indexers })
  const [term, setTerm] = useState('')
  const [indexer, setIndexer] = useState(ALL)
  const [category, setCategory] = useState(ALL)
  const [submitted, setSubmitted] = useState<Params | null>(null)
  const [sort, setSort] = useState<{ key: SortKey; desc: boolean }>({ key: 'relevancia', desc: true })

  const search = useQuery({
    queryKey: ['busca', submitted],
    queryFn: () => api.search(submitted!),
    enabled: submitted !== null,
    staleTime: 60_000,
  })

  const results = useMemo(() => {
    const list = [...(search.data?.resultados ?? [])]
    if (sort.key === 'relevancia') return list
    const value = (release: Release) =>
      sort.key === 'tamanho'
        ? release.tamanho
        : sort.key === 'seeders'
          ? (release.seeders ?? -1)
          : -ageInSeconds(release.publicado)
    list.sort((a, b) => (sort.desc ? value(b) - value(a) : value(a) - value(b)))
    return list
  }, [search.data, sort])

  const toggleSort = (key: SortKey) =>
    setSort((current) => (current.key === key ? { key, desc: !current.desc } : { key, desc: true }))

  return (
    <>
      <PageHeader
        title="Busca"
        description="A mesma consulta que o Sonarr e o Radarr fazem. Sem termo, traz o mais recente de cada indexador."
      />

      <form
        role="search"
        className="mb-6 grid gap-3 rounded-lg border border-border bg-surface p-4 sm:grid-cols-[1fr_180px_150px_auto] sm:items-end"
        onSubmit={(event) => {
          event.preventDefault()
          setSubmitted({
            q: term.trim(),
            indexador: indexer === ALL ? '' : indexer,
            cat: category === ALL ? '' : category,
          })
        }}
      >
        <div className="grid gap-2">
          <Label htmlFor="termo">Termo</Label>
          <div className="relative">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-content-subtle" aria-hidden="true" />
            <Input
              id="termo"
              type="search"
              autoComplete="off"
              autoFocus
              placeholder="Título, ano, temporada…"
              value={term}
              onChange={(event) => setTerm(event.target.value)}
              className="pl-9"
            />
          </div>
        </div>
        <div className="grid gap-2">
          <Label htmlFor="filtro-indexador">Indexador</Label>
          <Select value={indexer} onValueChange={setIndexer}>
            <SelectTrigger id="filtro-indexador">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL}>Todos</SelectItem>
              {indexers.data?.indexadores.map((item) => (
                <SelectItem key={item.nome} value={item.nome}>
                  {item.nome}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-2">
          <Label htmlFor="filtro-categoria">Categoria</Label>
          <Select value={category} onValueChange={setCategory}>
            <SelectTrigger id="filtro-categoria">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {CATEGORIES.map((item) => (
                <SelectItem key={item.value} value={item.value}>
                  {item.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Button type="submit" variant="primary" loading={search.isFetching}>
          {!search.isFetching && <SearchIcon aria-hidden="true" />}
          Buscar
        </Button>
      </form>

      <div aria-live="polite">
        {submitted === null ? (
          <EmptyState
            icon={SearchIcon}
            title="Busque em todos os indexadores de uma vez"
            text="Digite um título, ou busque sem termo para ver o que chegou por último."
          />
        ) : search.isPending ? (
          <ResultsSkeleton />
        ) : search.isError ? (
          <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
            <p className="font-medium">A busca não pôde ser concluída.</p>
            <p className="text-sm text-content-muted">{search.error.message}</p>
            <Button onClick={() => void search.refetch()}>Tentar novamente</Button>
          </div>
        ) : (
          <>
            {search.data.falhas.length > 0 && (
              <div role="status" className="mb-4 flex gap-2 rounded-md bg-warning-bg px-3 py-2 text-sm text-warning">
                <CircleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                <div>
                  {search.data.falhas.map((failure) => (
                    <p key={failure.indexador}>
                      <span className="font-medium">{failure.indexador}</span> falhou: {failure.erro}
                    </p>
                  ))}
                </div>
              </div>
            )}
            {results.length === 0 ? (
              <EmptyState
                icon={SearchX}
                title={submitted.q ? `Nada encontrado para “${submitted.q}”` : 'Nada recente nos indexadores'}
                text="Tente outro termo, menos palavras ou todas as categorias."
              />
            ) : (
              <ResultsTable results={results} sort={sort} onSort={toggleSort} />
            )}
          </>
        )}
      </div>
    </>
  )
}

function EmptyState({ icon: Icon, title, text }: { icon: typeof SearchX; title: string; text: string }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
      <Icon className="size-8 text-content-subtle" aria-hidden="true" />
      <p className="font-medium">{title}</p>
      <p className="max-w-md text-sm text-content-muted">{text}</p>
    </div>
  )
}

function SortHeader({
  label,
  column,
  sort,
  onSort,
  className,
}: {
  label: string
  column: SortKey
  sort: { key: SortKey; desc: boolean }
  onSort: (key: SortKey) => void
  className?: string
}) {
  const active = sort.key === column
  const Icon = !active ? ArrowUpDown : sort.desc ? ArrowDown : ArrowUp
  return (
    <th
      scope="col"
      aria-sort={active ? (sort.desc ? 'descending' : 'ascending') : 'none'}
      className={cn('px-3 py-2.5 font-medium', className)}
    >
      <button
        type="button"
        onClick={() => onSort(column)}
        className={cn(
          'inline-flex items-center gap-1 rounded-sm transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-ring',
          active && 'text-content',
        )}
      >
        {label}
        <Icon className="size-3.5" aria-hidden="true" />
      </button>
    </th>
  )
}

function ResultsTable({
  results,
  sort,
  onSort,
}: {
  results: Release[]
  sort: { key: SortKey; desc: boolean }
  onSort: (key: SortKey) => void
}) {
  return (
    <div className="overflow-hidden rounded-lg border border-border bg-surface">
      <p className="border-b border-border px-4 py-2.5 text-sm text-content-muted">
        <span className="font-medium text-content tabular-nums">{formatCount(results.length)}</span> resultados
      </p>
      <ul className="divide-y divide-border md:hidden">
        {results.map((release, index) => (
          <ResultCard key={`${release.indexador}-${release.download}-${index}`} release={release} />
        ))}
      </ul>
      <div className="hidden md:block">
        <table className="w-full table-fixed text-sm">
          <thead className="bg-surface-raised text-left text-xs text-content-muted">
            <tr>
              <th scope="col" className="px-4 py-2.5 font-medium">
                Título
              </th>
              <SortHeader label="Tamanho" column="tamanho" sort={sort} onSort={onSort} className="w-28 text-right" />
              <SortHeader label="Seeders" column="seeders" sort={sort} onSort={onSort} className="w-24 text-right" />
              <SortHeader label="Idade" column="idade" sort={sort} onSort={onSort} className="w-32 text-right" />
              <th scope="col" className="w-12 px-3 py-2.5">
                <span className="sr-only">Baixar</span>
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {results.map((release, index) => (
              <ResultRow key={`${release.indexador}-${release.download}-${index}`} release={release} />
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function seedTone(seeders: number | null) {
  if (seeders == null) return 'text-content-subtle'
  if (seeders === 0) return 'text-danger'
  if (seeders < 5) return 'text-warning'
  return 'text-success'
}

function ResultRow({ release }: { release: Release }) {
  const details = safeHref(release.detalhes)
  const download = safeHref(release.download)
  const magnet = download?.startsWith('magnet:')
  const category = categoryLabel(release.categorias)
  return (
    <tr className="transition-colors hover:bg-surface-raised/60">
      <td className="px-4 py-3">
        <div className="flex items-start gap-2">
          <div className="min-w-0 flex-1">
            {details ? (
              <a
                href={details}
                target="_blank"
                rel="noopener noreferrer"
                className="group inline-flex max-w-full items-center gap-1 font-medium hover:text-accent"
              >
                <span className="line-clamp-2 break-words" title={release.titulo}>
                  {release.titulo}
                </span>
                <ExternalLink className="size-3.5 shrink-0 opacity-0 transition-opacity group-hover:opacity-100" aria-hidden="true" />
              </a>
            ) : (
              <span className="line-clamp-2 font-medium break-words" title={release.titulo}>
                {release.titulo}
              </span>
            )}
            <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs text-content-subtle">
              <Badge tone="accent">{release.indexador}</Badge>
              {category && <Badge>{category}</Badge>}
              {release.leechers != null && <span className="tabular-nums">{formatCount(release.leechers)} leechers</span>}
            </div>
          </div>
        </div>
      </td>
      <td className="px-3 py-3 text-right whitespace-nowrap tabular-nums">{formatSize(release.tamanho)}</td>
      <td className={cn('px-3 py-3 text-right font-medium whitespace-nowrap tabular-nums', seedTone(release.seeders))}>
        {formatCount(release.seeders)}
      </td>
      <td className="px-3 py-3 text-right whitespace-nowrap text-content-muted">
        {release.publicado ? (
          <time dateTime={release.publicado} title={new Date(release.publicado).toLocaleString('pt-BR')}>
            {formatAgo(release.publicado)}
          </time>
        ) : (
          '—'
        )}
      </td>
      <td className="px-3 py-3 text-right">
        {download && (
          <Tooltip content={magnet ? 'Abrir magnet' : 'Baixar .torrent'}>
            <Button asChild variant="ghost" size="icon-sm">
              <a href={download} download={magnet ? undefined : ''} aria-label={`Baixar ${release.titulo}`}>
                {magnet ? <Magnet aria-hidden="true" /> : <Download aria-hidden="true" />}
              </a>
            </Button>
          </Tooltip>
        )}
      </td>
    </tr>
  )
}

/** Resultado no celular: título inteiro em cima, números embaixo. */
function ResultCard({ release }: { release: Release }) {
  const details = safeHref(release.detalhes)
  const download = safeHref(release.download)
  const magnet = download?.startsWith('magnet:')
  const category = categoryLabel(release.categorias)
  return (
    <li className="flex gap-3 px-4 py-3.5">
      <div className="min-w-0 flex-1">
        {details ? (
          <a href={details} target="_blank" rel="noopener noreferrer" className="font-medium break-words hover:text-accent">
            {release.titulo}
          </a>
        ) : (
          <p className="font-medium break-words">{release.titulo}</p>
        )}
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
          <Badge tone="accent">{release.indexador}</Badge>
          {category && <Badge>{category}</Badge>}
        </div>
        <p className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs text-content-muted tabular-nums">
          <span>{formatSize(release.tamanho)}</span>
          <span className={cn('font-medium', seedTone(release.seeders))}>{formatCount(release.seeders)} seeders</span>
          {release.publicado && <span>{formatAgo(release.publicado)}</span>}
        </p>
      </div>
      {download && (
        <Button asChild variant="secondary" size="icon">
          <a href={download} download={magnet ? undefined : ''} aria-label={`Baixar ${release.titulo}`}>
            {magnet ? <Magnet aria-hidden="true" /> : <Download aria-hidden="true" />}
          </a>
        </Button>
      )}
    </li>
  )
}

function ResultsSkeleton() {
  return (
    <div aria-busy="true" aria-label="Buscando" className="overflow-hidden rounded-lg border border-border bg-surface">
      <p className="flex items-center gap-2 border-b border-border px-4 py-2.5 text-sm text-content-muted">
        <Loader2 className="size-4 animate-spin" aria-hidden="true" />
        Buscando nos indexadores… tracker lento com várias páginas pode levar meio minuto.
      </p>
      {Array.from({ length: 8 }, (_, index) => (
        <div key={index} className="flex items-center gap-4 border-b border-border px-4 py-3.5 last:border-0">
          <div className="flex-1 space-y-2">
            <Skeleton className="h-4 w-3/5" />
            <Skeleton className="h-3 w-40" />
          </div>
          <Skeleton className="h-4 w-16" />
          <Skeleton className="h-4 w-10" />
          <Skeleton className="h-4 w-16" />
          <Skeleton className="size-8" />
        </div>
      ))}
    </div>
  )
}
