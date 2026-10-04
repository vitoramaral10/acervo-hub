import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowDown,
  ArrowDownToLine,
  ArrowUp,
  ArrowUpDown,
  LayoutGrid,
  Table2,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  ChevronRight,
  ExternalLink,
  Film,
  ListFilter,
  LoaderCircle,
  Plus,
  Radar,
  ScanSearch,
  SearchX,
  Star,
  Trash2,
  FilePen,
} from 'lucide-react'
import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { AddMovieDialog } from '@/components/AddMovieDialog'
import { HistoryList } from '@/components/HistoryList'
import { InteractiveSearch } from '@/components/InteractiveSearch'
import { RenameDialog, VerifyAllMoviesDialog, VerifyDialog } from '@/components/MaintenanceDialogs'
import { PageHeader } from '@/components/PageHeader'
import { PriorityBadge, PriorityStar } from '@/components/PriorityStar'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import {
  type GrabReport,
  type Movie,
  type MovieChange,
  type MovieDownload,
  type LastSearch,
  api,
  library,
} from '@/lib/api'
import { formatAgo, formatCount, formatSize } from '@/lib/format'
import {
  AUDIOS,
  DEFAULT_PREFS,
  type MovieListPrefs,
  MONITORED,
  QUALITIES,
  SORTS,
  STATES,
  type SortKey,
  applyList,
  defaultDir,
  filterExceptState,
  hasActiveFilters,
  hasFailed,
  hasProblem,
  isDownloading,
  isQueued,
  loadPrefs,
  savePrefs,
  stateCounts,
} from '@/lib/movieList'
import { cn } from '@/lib/utils'

const DISK = {
  ok: { label: 'No disco', tone: 'success', icon: CircleCheck },
  missing: { label: 'Ausente do disco', tone: 'danger', icon: CircleAlert },
  size_differs: {
    label: 'Tamanho diferente',
    tone: 'warning',
    icon: CircleAlert,
  },
  unreadable: { label: 'Não deu para ler', tone: 'warning', icon: CircleAlert },
} as const

/** Motivos de rejeição, na voz da tela. */
const REASONS: Record<string, string> = {
  UnknownMovie: 'de outro filme',
  WrongMovie: 'de outro filme',
  UnableToParse: 'nome ilegível',
  QualityNotWanted: 'qualidade fora do perfil',
  WantedLanguage: 'sem o idioma original',
  BelowMinimumSize: 'pequeno demais',
  AboveMaximumSize: 'grande demais',
  MaximumSizeExceeded: 'acima do teto',
  MinimumSeeders: 'poucos seeders',
  Raw: 'disco bruto',
  HardcodeSubtitles: 'legenda embutida',
  Sample: 'amostra',
  QueueCutoffMet: 'já baixando',
  QueueHigherPreference: 'já baixando',
  QueueUpgradesNotAllowed: 'já baixando',
}

function LastSearchLine({ search }: { search: LastSearch }) {
  const when = formatAgo(search.quando)
  if (search.erro) {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-danger">
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Última busca {when}: falhou — {search.erro}
        </span>
      </p>
    )
  }
  if (search.escolhido) {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-accent" title={search.escolhido}>
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Última busca {when}: escolheu <span className="font-mono">{search.escolhido}</span>
        </span>
      </p>
    )
  }
  const reasons =
    search.releases === 0
      ? 'nenhum resultado'
      : search.motivos.map(([reason, count]) => `${REASONS[reason] ?? reason} (${count})`).join(', ')
  return (
    <p className="mt-1 flex items-center gap-1.5 text-xs text-content-subtle">
      <Radar className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="truncate">
        Última busca {when}: nada entre {formatCount(search.releases)} — {reasons}
      </span>
    </p>
  )
}

function DownloadLine({ download }: { download: MovieDownload }) {
  const when = formatAgo(download.pego_em)
  if (download.estado === 'failed') {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-danger" title={download.release}>
        <CircleAlert className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Download {when} falhou
          {download.mensagem ? ` — ${download.mensagem}` : ''}
        </span>
      </p>
    )
  }
  if (download.estado === 'imported') {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-success" title={download.release}>
        <CircleCheck className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">Importado</span>
      </p>
    )
  }
  return (
    <p className="mt-1 flex items-center gap-1.5 text-xs text-accent" title={download.release}>
      <LoaderCircle className="size-3.5 shrink-0 animate-spin motion-reduce:animate-none" aria-hidden="true" />
      <span className="truncate">
        Baixando{download.mensagem ? ` (${download.mensagem})` : ''}:{' '}
        <span className="font-mono">{download.release}</span>
      </span>
    </p>
  )
}

/** Busca, mostra a escolha e só pega depois de confirmar. */
function GrabDialog({
  movie,
  open,
  onOpenChange,
}: {
  movie: Movie
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [plan, setPlan] = useState<GrabReport | null>(null)
  const search = useMutation({
    mutationFn: () => api.grab(movie.id, false),
    onSuccess: setPlan,
  })
  const take = useMutation({
    mutationFn: () => api.grab(movie.id, true),
    onSuccess: (report) => {
      if (report.aplicado && report.escolhido) {
        toast.success(`${report.filme}: mandado ao qBittorrent`)
        onOpenChange(false)
      } else {
        // A escolha mudou entre a busca e a confirmação.
        setPlan(report)
      }
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
  })
  const pick = plan?.escolhido

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (next) {
          setPlan(null)
          search.mutate()
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            Pegar {movie.titulo}
            {movie.ano ? ` (${movie.ano})` : ''}
          </DialogTitle>
          <DialogDescription>
            Busca em todos os indexadores e escolhe o melhor release. Nada é baixado antes de você confirmar.
          </DialogDescription>
        </DialogHeader>
        <div className="min-h-24" aria-live="polite">
          {search.isPending ? (
            <div className="grid gap-2" aria-busy="true">
              <Skeleton className="h-4 w-3/4" />
              <Skeleton className="h-4 w-1/2" />
              <p className="text-xs text-content-subtle">Buscando nos indexadores…</p>
            </div>
          ) : search.isError ? (
            <p role="alert" className="text-sm text-danger">
              {search.error.message}
            </p>
          ) : pick && plan ? (
            <dl className="grid gap-3 rounded-md border border-border p-4 text-sm">
              <div>
                <dt className="text-xs text-content-subtle">Release</dt>
                <dd className="font-mono text-xs break-all">{pick.titulo}</dd>
              </div>
              <div className="flex flex-wrap gap-x-6 gap-y-2">
                <div>
                  <dt className="text-xs text-content-subtle">Indexador</dt>
                  <dd>{pick.indexador}</dd>
                </div>
                <div>
                  <dt className="text-xs text-content-subtle">Qualidade</dt>
                  <dd>{pick.qualidade}</dd>
                </div>
                <div>
                  <dt className="text-xs text-content-subtle">Tamanho</dt>
                  <dd className="tabular-nums">{formatSize(pick.tamanho)}</dd>
                </div>
              </div>
              <p className="text-xs text-content-subtle">Melhor de {formatCount(plan.releases)} releases.</p>
            </dl>
          ) : plan ? (
            <p className="text-sm text-content-muted">
              Nenhum release serve entre {formatCount(plan.releases)}
              {plan.motivos.length > 0 &&
                ` — ${plan.motivos.map(([reason, count]) => `${REASONS[reason] ?? reason} (${count})`).join(', ')}`}
              .
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancelar
          </Button>
          <Button variant="primary" disabled={!pick} loading={take.isPending} onClick={() => take.mutate()}>
            {!take.isPending && <ArrowDownToLine aria-hidden="true" />}
            Pegar este
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function MoviesPage() {
  const queryClient = useQueryClient()
  const movies = useQuery({ queryKey: ['filmes'], queryFn: api.movies })
  const [query, setQuery] = useState('')
  const [prefs, setPrefs] = useState<MovieListPrefs>(loadPrefs)
  const [selected, setSelected] = useState<number | null>(null)
  const [adding, setAdding] = useState(false)
  const [verifyingAll, setVerifyingAll] = useState(false)

  // A busca dos que faltam roda no servidor; a tela só acompanha enquanto ela dura.
  const progress = useQuery({
    queryKey: ['filmes-busca'],
    queryFn: api.missingSearch,
    refetchInterval: (state) => (state.state.data?.rodando ? 2000 : false),
  })
  const searching = progress.data?.rodando ?? false
  const searched = progress.data?.buscados ?? 0
  const wasSearching = useRef(false)
  useEffect(() => {
    // Cada filme buscado grava a "última busca" dele: recarrega a lista no caminho e ao fim.
    if (searching || wasSearching.current) void queryClient.invalidateQueries({ queryKey: ['filmes'] })
    if (wasSearching.current && !searching) toast.success('Busca dos que faltam concluída')
    wasSearching.current = searching
  }, [searching, searched, queryClient])

  const missing = useMutation({
    mutationFn: api.searchMissing,
    onSuccess: (status) => {
      queryClient.setQueryData(['filmes-busca'], status)
      if (!status.iniciada) toast.info('Já há uma busca dos que faltam em andamento')
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const list = movies.data?.filmes
  const counts = useMemo(() => {
    const all = list ?? []
    return {
      total: all.length,
      withFile: all.filter((m) => m.arquivo).length,
      problems: all.filter(hasProblem).length,
      size: all.reduce((sum, m) => sum + (m.arquivo?.tamanho ?? 0), 0),
    }
  }, [list])

  // Ordem, filtros e visualização vão para a URL e para o localStorage a cada mudança.
  useEffect(() => savePrefs(prefs), [prefs])
  const update = (change: Partial<MovieListPrefs>) => setPrefs((old) => ({ ...old, ...change }))

  const visible = useMemo(() => applyList(list ?? [], query, prefs), [list, query, prefs])
  // Contadores do estado: lista já cortada pela busca e pelos outros filtros, menos o próprio estado.
  const stateTotals = useMemo(() => stateCounts(filterExceptState(list ?? [], query, prefs)), [list, query, prefs])

  const current = list?.find((movie) => movie.id === selected) ?? null

  return (
    <>
      <PageHeader
        title="Filmes"
        description={
          movies.isSuccess
            ? `${formatCount(counts.total)} filmes, ${formatCount(counts.withFile)} no disco (${formatSize(counts.size)}).`
            : 'Sua biblioteca de filmes.'
        }
        action={
          <div className="flex flex-wrap gap-2">
            <Button
              variant="ghost"
              onClick={() => missing.mutate()}
              loading={missing.isPending || searching}
              disabled={counts.total === 0}
            >
              {!(missing.isPending || searching) && <Radar aria-hidden="true" />}
              {searching && (progress.data?.total ?? 0) > 0
                ? `Buscando ${progress.data?.buscados} de ${progress.data?.total}`
                : 'Buscar os que faltam'}
            </Button>
            <Button variant="ghost" onClick={() => setVerifyingAll(true)} disabled={counts.total === 0}>
              <ScanSearch aria-hidden="true" />
              Verificar todos os filmes
            </Button>
            <Button variant="primary" onClick={() => setAdding(true)}>
              <Plus aria-hidden="true" />
              Adicionar filme
            </Button>
          </div>
        }
      />
      {verifyingAll && <VerifyAllMoviesDialog onClose={() => setVerifyingAll(false)} />}
      <AddMovieDialog
        open={adding}
        onOpenChange={setAdding}
        onOpenMovie={(id) => {
          setAdding(false)
          setSelected(id)
        }}
      />

      {movies.isPending ? (
        <LibrarySkeleton />
      ) : movies.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível ler o catálogo.</p>
          <p className="text-sm text-content-muted">{movies.error.message}</p>
          <Button onClick={() => void movies.refetch()}>Tentar novamente</Button>
        </div>
      ) : counts.total === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <Film className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">A biblioteca está vazia</p>
          <p className="max-w-md text-sm text-content-muted">
            Adicione filmes buscando no TMDB.
          </p>
          <div className="flex flex-wrap justify-center gap-2">
            <Button variant="primary" onClick={() => setAdding(true)}>
              <Plus aria-hidden="true" />
              Adicionar filme
            </Button>
          </div>
        </div>
      ) : (
        <>
          <MovieToolbar
            query={query}
            onQuery={setQuery}
            prefs={prefs}
            onChange={update}
            stateTotals={stateTotals}
          />

          {visible.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
              <SearchX className="size-6 text-content-subtle" aria-hidden="true" />
              <p className="font-medium">Nenhum filme com esse filtro</p>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setQuery('')
                  update({ ...CLEARED })
                }}
              >
                Limpar filtros
              </Button>
            </div>
          ) : prefs.view === 'tabela' ? (
            <MovieTable
              movies={visible}
              sort={prefs.sort}
              dir={prefs.dir}
              onSort={(sort) =>
                update(
                  sort === prefs.sort
                    ? { dir: prefs.dir === 'asc' ? 'desc' : 'asc' }
                    : { sort, dir: defaultDir(sort) },
                )
              }
              onOpen={setSelected}
            />
          ) : (
            <ul className={GRID} aria-label="Filmes">
              {visible.map((movie) => (
                <li key={movie.id}>
                  <PosterCard movie={movie} onOpen={() => setSelected(movie.id)} />
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

      {current && (
        <MovieDetails movie={current} open={selected !== null} onOpenChange={(open) => !open && setSelected(null)} />
      )}
    </>
  )
}

const CLEARED = {
  state: DEFAULT_PREFS.state,
  qualities: DEFAULT_PREFS.qualities,
  audio: DEFAULT_PREFS.audio,
  monitored: DEFAULT_PREFS.monitored,
  priority: DEFAULT_PREFS.priority,
}

const CHIP =
  'inline-flex h-7 items-center gap-1 rounded-sm px-2 text-xs font-medium whitespace-nowrap text-content-muted transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring'

/** Grupo de opções num contorno só; `single` vira radiogroup, o resto são botões de alternar. */
export function ChipGroup({
  label,
  children,
  single = false,
  nowrap = false,
}: {
  label: string
  children: ReactNode
  single?: boolean
  nowrap?: boolean
}) {
  return (
    <div
      role={single ? 'radiogroup' : 'group'}
      aria-label={label}
      className={cn('inline-flex rounded-md border border-border p-0.5', nowrap ? 'flex-nowrap' : 'flex-wrap')}
    >
      {children}
    </div>
  )
}

export function Chip({
  active,
  single = false,
  onClick,
  title,
  children,
}: {
  active: boolean
  single?: boolean
  onClick: () => void
  title?: string
  children: ReactNode
}) {
  return (
    <button
      type="button"
      title={title}
      role={single ? 'radio' : undefined}
      aria-checked={single ? active : undefined}
      aria-pressed={single ? undefined : active}
      onClick={onClick}
      className={cn(CHIP, active && 'bg-surface-raised text-content')}
    >
      {children}
    </button>
  )
}

function toggle<T>(list: T[], value: T): T[] {
  return list.includes(value) ? list.filter((item) => item !== value) : [...list, value]
}

function MovieToolbar({
  query,
  onQuery,
  prefs,
  onChange,
  stateTotals,
}: {
  query: string
  onQuery: (value: string) => void
  prefs: MovieListPrefs
  onChange: (change: Partial<MovieListPrefs>) => void
  stateTotals: Record<(typeof STATES)[number]['value'], number>
}) {
  const DirIcon = prefs.dir === 'asc' ? ArrowUp : ArrowDown
  return (
    <div className="mb-5 flex flex-wrap items-center gap-2 lg:flex-nowrap">
        <label className="min-w-0 flex-1 basis-56 sm:max-w-xs lg:w-64 lg:flex-none">
          <span className="sr-only">Buscar na biblioteca</span>
          <Input
            type="search"
            value={query}
            onChange={(event) => onQuery(event.target.value)}
            placeholder="Buscar por título, ano ou IMDb"
            className="h-8"
          />
        </label>
        <div className="min-w-0 flex-1 overflow-x-auto">
          <ChipGroup label="Estado" single nowrap>
            {STATES.filter((s) => s.value !== 'problemas' || stateTotals.problemas > 0 || prefs.state === 'problemas').map(
              ({ value, label }) => (
                <Chip key={value} single active={prefs.state === value} onClick={() => onChange({ state: value })}>
                  {label}
                  <span className={cn('tabular-nums', value === 'problemas' && stateTotals[value] > 0 ? 'text-danger' : 'text-content-subtle')}>
                    {formatCount(stateTotals[value])}
                  </span>
                </Chip>
              ),
            )}
          </ChipGroup>
        </div>
        <ChipGroup label="Prioridade">
          <Chip active={prefs.priority} onClick={() => onChange({ priority: !prefs.priority })}>
            <Star className={cn('size-3.5', prefs.priority && 'fill-warning text-warning')} aria-hidden="true" />
            Prioritários
          </Chip>
        </ChipGroup>
        <div className="flex shrink-0 items-center gap-1.5">
          <Select
            value={prefs.sort}
            onValueChange={(value) => onChange({ sort: value as SortKey, dir: defaultDir(value as SortKey) })}
          >
            <SelectTrigger aria-label="Ordenar por" className="h-8 w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SORTS.map(({ value, label }) => (
                <SelectItem key={value} value={value}>
                  {label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            variant="secondary"
            size="icon-sm"
            onClick={() => onChange({ dir: prefs.dir === 'asc' ? 'desc' : 'asc' })}
            aria-label={`Inverter a ordem (agora ${prefs.dir === 'asc' ? 'crescente' : 'decrescente'})`}
            title={prefs.dir === 'asc' ? 'Crescente' : 'Decrescente'}
          >
            <DirIcon aria-hidden="true" />
          </Button>
          <FilterMenu prefs={prefs} onChange={onChange} />
          <ChipGroup label="Visualização" single>
            <Chip single active={prefs.view === 'poster'} onClick={() => onChange({ view: 'poster' })} title="Pôster">
              <LayoutGrid className="size-4" aria-hidden="true" />
              <span className="sr-only">Pôster</span>
            </Chip>
            <Chip single active={prefs.view === 'tabela'} onClick={() => onChange({ view: 'tabela' })} title="Tabela">
              <Table2 className="size-4" aria-hidden="true" />
              <span className="sr-only">Tabela</span>
            </Chip>
          </ChipGroup>
        </div>
    </div>
  )
}

/** Qualidade, áudio e monitoramento num painel só, aberto por um botão: os
 * filtros menos usados não ocupam linha na tela. */
function FilterMenu({
  prefs,
  onChange,
}: {
  prefs: MovieListPrefs
  onChange: (change: Partial<MovieListPrefs>) => void
}) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const active = prefs.qualities.length + prefs.audio.length + (prefs.monitored === 'todos' ? 0 : 1)
  useEffect(() => {
    if (!open) return
    const outside = (event: PointerEvent) => {
      if (root.current && !root.current.contains(event.target as Node)) setOpen(false)
    }
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', outside)
    document.addEventListener('keydown', escape)
    return () => {
      document.removeEventListener('pointerdown', outside)
      document.removeEventListener('keydown', escape)
    }
  }, [open])
  return (
    <div ref={root} className="relative">
      <Button
        variant="secondary"
        size="sm"
        aria-expanded={open}
        aria-controls="filmes-filtros"
        onClick={() => setOpen((value) => !value)}
      >
        <ListFilter aria-hidden="true" />
        Filtros
        {active > 0 && <span className="rounded-full bg-accent px-1.5 text-xs text-accent-fg tabular-nums">{active}</span>}
      </Button>
      {open && (
        <div
          id="filmes-filtros"
          className="absolute right-0 z-20 mt-1 flex w-72 flex-col gap-3 rounded-md border border-border bg-surface-raised p-3 shadow-lg"
        >
          <FilterRow label="Qualidade">
            {QUALITIES.map(({ value, label }) => (
              <Chip key={value} active={prefs.qualities.includes(value)} onClick={() => onChange({ qualities: toggle(prefs.qualities, value) })}>
                {label}
              </Chip>
            ))}
          </FilterRow>
          <FilterRow label="Áudio">
            {AUDIOS.map(({ value, label }) => (
              <Chip key={value} active={prefs.audio.includes(value)} onClick={() => onChange({ audio: toggle(prefs.audio, value) })}>
                {label}
              </Chip>
            ))}
          </FilterRow>
          <FilterRow label="Monitoramento" single>
            {MONITORED.map(({ value, label }) => (
              <Chip key={value} single active={prefs.monitored === value} onClick={() => onChange({ monitored: value })}>
                {label}
              </Chip>
            ))}
          </FilterRow>
          {hasActiveFilters(prefs) && (
            <Button variant="ghost" size="sm" className="self-start" onClick={() => onChange({ ...CLEARED })}>
              Limpar filtros
            </Button>
          )}
        </div>
      )}
    </div>
  )
}

function FilterRow({ label, single = false, children }: { label: string; single?: boolean; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs text-content-subtle">{label}</span>
      <ChipGroup label={label} single={single}>
        {children}
      </ChipGroup>
    </div>
  )
}

/** Estado do filme numa palavra, para a tabela. */
function StateBadge({ movie }: { movie: Movie }) {
  if (hasProblem(movie)) {
    const disk = DISK[movie.arquivo!.disco]
    return (
      <Badge tone={disk.tone}>
        <disk.icon aria-hidden="true" />
        {disk.label}
      </Badge>
    )
  }
  if (movie.arquivo) {
    return (
      <Badge tone="success">
        <CircleCheck aria-hidden="true" />
        No disco
      </Badge>
    )
  }
  if (isQueued(movie)) {
    return (
      <Badge>
        <CircleDashed aria-hidden="true" />
        Na fila
      </Badge>
    )
  }
  if (isDownloading(movie)) {
    return (
      <Badge tone="accent">
        <LoaderCircle className="animate-spin motion-reduce:animate-none" aria-hidden="true" />
        Baixando
      </Badge>
    )
  }
  if (hasFailed(movie)) {
    return (
      <Badge tone="danger">
        <CircleAlert aria-hidden="true" />
        Falhou
      </Badge>
    )
  }
  return (
    <Badge>
      <CircleDashed aria-hidden="true" />
      {movie.monitorado ? 'Faltando' : 'Não monitorado'}
    </Badge>
  )
}

const dateFormat = new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: 'short', year: 'numeric' })

function formatDay(iso: string | null | undefined): string {
  if (!iso) return '—'
  const date = new Date(iso)
  return Number.isNaN(date.getTime()) ? '—' : dateFormat.format(date)
}

const COLUMNS: { label: string; sort: SortKey | null; align: 'left' | 'right' }[] = [
  { label: 'Título', sort: 'titulo', align: 'left' },
  { label: 'Ano', sort: 'ano', align: 'right' },
  { label: 'Qualidade', sort: 'qualidade', align: 'left' },
  { label: 'Tamanho', sort: 'tamanho', align: 'right' },
  { label: 'Estado', sort: null, align: 'left' },
  { label: 'Adicionado', sort: 'adicionado', align: 'right' },
]

function MovieTable({
  movies,
  sort,
  dir,
  onSort,
  onOpen,
}: {
  movies: Movie[]
  sort: SortKey
  dir: 'asc' | 'desc'
  onSort: (sort: SortKey) => void
  onOpen: (id: number) => void
}) {
  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full min-w-[42rem] border-collapse text-sm">
        <caption className="sr-only">Filmes</caption>
        <thead className="bg-surface-raised text-xs text-content-muted">
          <tr>
            {COLUMNS.map(({ label, sort: key, align }) => {
              const active = key !== null && key === sort
              const Icon = active ? (dir === 'asc' ? ArrowUp : ArrowDown) : ArrowUpDown
              return (
                <th
                  key={label}
                  scope="col"
                  aria-sort={active ? (dir === 'asc' ? 'ascending' : 'descending') : key ? 'none' : undefined}
                  className={cn('px-3 py-2 font-medium whitespace-nowrap', align === 'right' ? 'text-right' : 'text-left')}
                >
                  {key ? (
                    <button
                      type="button"
                      onClick={() => onSort(key)}
                      className={cn(
                        'inline-flex items-center gap-1 rounded-sm hover:text-content focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring',
                        active && 'text-content',
                      )}
                    >
                      {label}
                      <Icon className={cn('size-3.5', !active && 'opacity-50')} aria-hidden="true" />
                    </button>
                  ) : (
                    label
                  )}
                </th>
              )
            })}
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {movies.map((movie) => (
            <tr
              key={movie.id}
              onClick={() => onOpen(movie.id)}
              className="cursor-pointer bg-surface transition-colors hover:bg-surface-raised"
            >
              <td className="max-w-[22rem] px-3 py-2">
                {/* O botão é o alvo de teclado; o clique na linha inteira é só conveniência do mouse. */}
                <button
                  type="button"
                  onClick={(event) => {
                    event.stopPropagation()
                    onOpen(movie.id)
                  }}
                  className="block max-w-full truncate rounded-sm text-left font-medium focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                  title={movie.titulo}
                >
                  {movie.prioritario && (
                    <Star className="mr-1 inline size-3.5 fill-warning text-warning" aria-label="Prioritário" />
                  )}
                  {movie.titulo}
                </button>
              </td>
              <td className="px-3 py-2 text-right text-content-muted tabular-nums">{movie.ano ?? '—'}</td>
              <td className="px-3 py-2 whitespace-nowrap text-content-muted">{movie.arquivo?.qualidade ?? '—'}</td>
              <td className="px-3 py-2 text-right whitespace-nowrap text-content-muted tabular-nums">
                {movie.arquivo ? formatSize(movie.arquivo.tamanho) : '—'}
              </td>
              <td className="px-3 py-2">
                <StateBadge movie={movie} />
              </td>
              <td className="px-3 py-2 text-right whitespace-nowrap text-content-muted tabular-nums">
                {formatDay(movie.adicionado)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

export const GRID =
  'grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-x-3 gap-y-5 sm:gap-x-4 sm:gap-y-6 sm:grid-cols-[repeat(auto-fill,minmax(10rem,1fr))]'

const STATUS: Record<string, string> = {
  announced: 'Anunciado',
  inCinemas: 'No cinema',
  released: 'Lançado',
}

function Poster({ movie, className }: { movie: Movie; className?: string }) {
  const [failed, setFailed] = useState(false)
  return (
    <div
      className={cn(
        'relative aspect-[2/3] overflow-hidden rounded-md border border-border bg-surface-raised',
        className,
      )}
    >
      {movie.poster && !failed ? (
        <img
          src={movie.poster}
          alt=""
          loading="lazy"
          decoding="async"
          onError={() => setFailed(true)}
          className="size-full object-cover"
        />
      ) : (
        <div className="flex size-full flex-col items-center justify-center gap-2 p-3 text-center">
          <Film className="size-6 text-content-subtle" aria-hidden="true" />
          <span className="line-clamp-3 text-xs text-content-muted">{movie.titulo}</span>
        </div>
      )}
    </div>
  )
}

/** O que falta dizer sobre o filme na capa; filme no disco não precisa de marca. */
function PosterMark({ movie }: { movie: Movie }) {
  if (hasProblem(movie)) {
    const disk = DISK[movie.arquivo!.disco]
    return (
      <Badge tone={disk.tone} className="shadow-sm">
        <disk.icon aria-hidden="true" />
        {disk.label}
      </Badge>
    )
  }
  if (movie.arquivo) return null
  if (movie.download?.estado === 'downloading') {
    return (
      <Badge tone="accent" className="shadow-sm">
        <LoaderCircle className="animate-spin motion-reduce:animate-none" aria-hidden="true" />
        Baixando
      </Badge>
    )
  }
  if (movie.download?.estado === 'failed') {
    return (
      <Badge tone="danger" className="shadow-sm">
        <CircleAlert aria-hidden="true" />
        Falhou
      </Badge>
    )
  }
  return (
    <Badge className="shadow-sm">
      <CircleDashed aria-hidden="true" />
      {movie.monitorado ? 'Faltando' : 'Não monitorado'}
    </Badge>
  )
}

function PosterCard({ movie, onOpen }: { movie: Movie; onOpen: () => void }) {
  const missing = !movie.arquivo
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group block w-full rounded-md text-left focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-ring"
    >
      <div className="relative">
        <Poster
          movie={movie}
          className={cn(
            'transition-[transform,opacity] duration-150 ease-out group-hover:-translate-y-0.5 group-hover:border-border-strong motion-reduce:transform-none',
            missing && 'opacity-60 group-hover:opacity-100',
          )}
        />
        <div className="absolute top-2 left-2 max-w-[calc(100%-1rem)]">
          <PosterMark movie={movie} />
        </div>
        {movie.prioritario && <PriorityBadge className="absolute top-2 right-2" />}
      </div>
      <p className="mt-2 line-clamp-2 text-sm leading-snug font-medium">{movie.titulo}</p>
      <p className="mt-0.5 text-xs text-content-subtle tabular-nums">
        {movie.ano ?? '—'}
        {movie.arquivo && ` · ${movie.arquivo.qualidade}`}
      </p>
    </button>
  )
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <dt className="text-xs text-content-subtle">{label}</dt>
      <dd className="mt-0.5 text-sm">{children}</dd>
    </div>
  )
}

function RemoveMovieDialog({
  movie,
  open,
  onOpenChange,
  onRemoved,
}: {
  movie: Movie
  open: boolean
  onOpenChange: (open: boolean) => void
  onRemoved: () => void
}) {
  const queryClient = useQueryClient()
  const [deleteFiles, setDeleteFiles] = useState(true)
  const remove = useMutation({
    mutationFn: () => library.remove(movie.id, { apagar_arquivos: deleteFiles }),
    onSuccess: () => {
      toast.success(`${movie.titulo} removido`)
      onRemoved()
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Remover {movie.titulo}?</DialogTitle>
          <DialogDescription>O filme sai da biblioteca.</DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="flex items-start justify-between gap-4">
            <div>
              <Label htmlFor="remover-arquivos">Apagar os arquivos e o download</Label>
              <p className="text-xs text-content-subtle">
                A pasta do filme e o torrent no qBittorrent, com os dados, na hora.
              </p>
              <p className="font-mono text-xs break-all text-content-subtle">{movie.pasta}</p>
            </div>
            <Switch id="remover-arquivos" checked={deleteFiles} onCheckedChange={setDeleteFiles} />
          </div>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancelar
          </Button>
          <Button variant="danger" loading={remove.isPending} onClick={() => remove.mutate()}>
            <Trash2 aria-hidden="true" />
            {deleteFiles ? 'Remover e apagar' : 'Remover'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function MovieSettings({ movie }: { movie: Movie }) {
  const queryClient = useQueryClient()
  const edit = useMutation({
    mutationFn: (change: MovieChange) => library.edit(movie.id, change),
    onSuccess: () => toast.success('Filme atualizado'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
  })
  return (
    <div className="grid gap-4 rounded-md border border-border p-4 sm:grid-cols-2">
      <p className="text-xs text-content-subtle sm:col-span-2">
        Qualidade automática: pega a melhor entre as que têm 5 ou mais seeders (do Remux 2160p ao SD), e não troca depois.
      </p>
      <div className="flex items-center justify-between gap-3 sm:flex-col sm:items-start">
        <Label htmlFor={`monitorado-${movie.id}`}>Monitorado</Label>
        <Switch
          id={`monitorado-${movie.id}`}
          checked={movie.monitorado}
          disabled={edit.isPending}
          onCheckedChange={(on) => edit.mutate({ monitorado: on })}
        />
      </div>
    </div>
  )
}

function MovieHistory({ movie }: { movie: Movie }) {
  const history = useQuery({
    queryKey: ['historico', 'filme', movie.id],
    queryFn: () => library.movieHistory(movie.id),
  })
  if (history.isPending) return <Skeleton className="h-20" />
  if (history.isError)
    return (
      <p role="alert" className="text-sm text-danger">
        {history.error.message}
      </p>
    )
  if (history.data.eventos.length === 0) return <p className="text-sm text-content-subtle">Nada registrado ainda.</p>
  return <HistoryList events={history.data.eventos} showMovie={false} />
}

export function MovieDetails({
  movie,
  open,
  onOpenChange,
}: {
  movie: Movie
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [grabbing, setGrabbing] = useState(false)
  const [interactive, setInteractive] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [renaming, setRenaming] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [showHistory, setShowHistory] = useState(false)
  const file = movie.arquivo
  const disk = file ? DISK[file.disco] : null
  const imdb = movie.imdb ? `https://www.imdb.com/title/${movie.imdb}/` : null
  const downloading = movie.download?.estado === 'downloading'
  const deleteFile = useMutation({
    mutationFn: () => library.deleteFile(movie.id),
    onSuccess: () => toast.success('Arquivo apagado'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
  })
  const prioritize = useMutation({
    mutationFn: (prioritario: boolean) => library.edit(movie.id, { prioritario }),
    onSuccess: (_, prioritario) => toast.success(prioritario ? 'Filme marcado como prioritário' : 'Prioridade removida'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
      void queryClient.invalidateQueries({ queryKey: ['faltando'] })
      void queryClient.invalidateQueries({ queryKey: ['calendario'] })
    },
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl gap-0 p-0">
        <div className="grid gap-6 p-6 sm:grid-cols-[12rem_1fr]">
          <Poster movie={movie} className="mx-auto w-40 sm:w-full" />
          <div className="flex min-w-0 flex-col gap-4">
            <DialogHeader>
              <DialogTitle className="text-2xl">
                {movie.titulo}{' '}
                {movie.ano && <span className="font-normal text-content-subtle tabular-nums">({movie.ano})</span>}
              </DialogTitle>
              {movie.titulo_original && movie.titulo_original !== movie.titulo && (
                <p className="text-sm text-content-subtle">{movie.titulo_original}</p>
              )}
            </DialogHeader>
            <DialogDescription className="max-w-[65ch] leading-relaxed">
              {movie.sinopse || 'Sem sinopse.'}
            </DialogDescription>

            <dl className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3">
              {file ? (
                <>
                  <Fact label="Qualidade">{file.qualidade}</Fact>
                  <Fact label="Tamanho">
                    <span className="tabular-nums">{formatSize(file.tamanho)}</span>
                  </Fact>
                  {file.idiomas.length > 0 && <Fact label="Áudio">{file.idiomas.join(', ')}</Fact>}
                </>
              ) : (
                <Fact label="Arquivo">{movie.monitorado ? 'Faltando' : 'Não monitorado'}</Fact>
              )}
              {movie.status && <Fact label="Lançamento">{STATUS[movie.status] ?? movie.status}</Fact>}
              {disk && file && (
                <Fact label="Disco">
                  <span
                    className={cn(
                      'inline-flex items-center gap-1',
                      disk.tone === 'success'
                        ? 'text-success'
                        : disk.tone === 'danger'
                          ? 'text-danger'
                          : 'text-warning',
                    )}
                  >
                    <disk.icon className="size-3.5" aria-hidden="true" />
                    {disk.label}
                  </span>
                </Fact>
              )}
            </dl>

            {(file?.release || movie.download || movie.ultima_busca) && (
              <div className="border-t border-border pt-3">
                {file?.release && <p className="font-mono text-xs break-all text-content-subtle">{file.release}</p>}
                {file?.disco_detalhe && <p className="mt-1 text-xs text-warning">{file.disco_detalhe}</p>}
                {!file && movie.download ? (
                  <DownloadLine download={movie.download} />
                ) : (
                  !file && movie.ultima_busca && <LastSearchLine search={movie.ultima_busca} />
                )}
              </div>
            )}
          </div>
          <div className="grid gap-4 sm:col-span-2">
            <MovieSettings movie={movie} />
            <div>
              <button
                type="button"
                aria-expanded={showHistory}
                onClick={() => setShowHistory((v) => !v)}
                className="flex items-center gap-1.5 rounded-sm text-sm font-medium text-content-muted hover:text-content focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
              >
                <ChevronRight
                  className={cn(
                    'size-4 transition-transform motion-reduce:transition-none',
                    showHistory && 'rotate-90',
                  )}
                  aria-hidden="true"
                />
                Histórico do filme
              </button>
              {showHistory && (
                <div className="mt-2">
                  <MovieHistory movie={movie} />
                </div>
              )}
            </div>
          </div>
        </div>
        <DialogFooter className="flex-wrap border-t border-border px-6 py-4 sm:justify-between">
          <div className="flex flex-wrap gap-2">
            <Button variant="ghost" onClick={() => setRemoving(true)}>
              <Trash2 aria-hidden="true" />
              Remover
            </Button>
            <PriorityStar
              active={movie.prioritario}
              busy={prioritize.isPending}
              onToggle={(next) => prioritize.mutate(next)}
            />
            <Button variant="ghost" onClick={() => setVerifying(true)}>
              <ScanSearch aria-hidden="true" />
              Verificar disco
            </Button>
            {file && (
              <Button variant="ghost" onClick={() => setRenaming(true)}>
                <FilePen aria-hidden="true" />
                Renomear
              </Button>
            )}
            {file && (
              <Button variant="ghost" loading={deleteFile.isPending} onClick={() => deleteFile.mutate()}>
                Apagar arquivo
              </Button>
            )}
            {imdb && (
              <Button asChild variant="ghost">
                <a href={imdb} target="_blank" rel="noreferrer">
                  IMDb
                  <ExternalLink aria-hidden="true" />
                </a>
              </Button>
            )}
          </div>
          <div className="flex flex-wrap gap-2">
            <Button onClick={() => setInteractive(true)}>
              <ListFilter aria-hidden="true" />
              Busca interativa
            </Button>
            {!downloading && (
              <Button variant="primary" onClick={() => setGrabbing(true)}>
                <ArrowDownToLine aria-hidden="true" />
                {file ? 'Buscar versão melhor' : 'Pegar agora'}
              </Button>
            )}
          </div>
        </DialogFooter>
        {!downloading && <GrabDialog movie={movie} open={grabbing} onOpenChange={setGrabbing} />}
        {renaming && <RenameDialog kind="filme" id={movie.id} title={movie.titulo} onClose={() => setRenaming(false)} />}
        {verifying && <VerifyDialog kind="filme" id={movie.id} title={movie.titulo} onClose={() => setVerifying(false)} />}
        <InteractiveSearch movie={movie} open={interactive} onOpenChange={setInteractive} />
        <RemoveMovieDialog
          movie={movie}
          open={removing}
          onOpenChange={setRemoving}
          onRemoved={() => {
            setRemoving(false)
            onOpenChange(false)
          }}
        />
      </DialogContent>
    </Dialog>
  )
}

function LibrarySkeleton() {
  return (
    <div aria-busy="true" aria-label="Carregando filmes">
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
