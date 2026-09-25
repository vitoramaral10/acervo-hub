import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowDownToLine,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CloudDownload,
  Film,
  HardDrive,
  LoaderCircle,
  Radar,
  SearchX,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { PageHeader } from '@/components/PageHeader'
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
import { Badge, Skeleton, Tooltip } from '@/components/ui/misc'
import { type GrabReport, type Movie, type MovieDownload, type MovieImport, type MovieShadow, api } from '@/lib/api'
import { formatAgo, formatCount, formatSize } from '@/lib/format'
import { cn } from '@/lib/utils'

type Filter = 'todos' | 'com-arquivo' | 'sem-arquivo' | 'problemas'

const FILTERS: { value: Filter; label: string }[] = [
  { value: 'todos', label: 'Todos' },
  { value: 'com-arquivo', label: 'Com arquivo' },
  { value: 'sem-arquivo', label: 'Sem arquivo' },
  { value: 'problemas', label: 'Disco não confirma' },
]

const DISK = {
  ok: { label: 'No disco', tone: 'success', icon: CircleCheck },
  missing: { label: 'Ausente do disco', tone: 'danger', icon: CircleAlert },
  size_differs: { label: 'Tamanho diferente', tone: 'warning', icon: CircleAlert },
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
  MinimumFreeSpace: 'disco sem espaço',
  Raw: 'disco bruto',
  HardcodeSubtitles: 'legenda embutida',
  Sample: 'amostra',
  QueueCutoffMet: 'já baixando',
  QueueHigherPreference: 'já baixando',
  QueueUpgradesNotAllowed: 'já baixando',
}

function ShadowLine({ shadow }: { shadow: MovieShadow }) {
  const when = formatAgo(shadow.quando)
  if (shadow.erro) {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-danger">
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">Sombra {when}: a busca falhou — {shadow.erro}</span>
      </p>
    )
  }
  if (shadow.pegaria) {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-accent" title={shadow.pegaria}>
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Sombra {when}: pegaria <span className="font-mono">{shadow.pegaria}</span>
        </span>
      </p>
    )
  }
  const reasons =
    shadow.releases === 0
      ? 'nenhum resultado'
      : shadow.motivos
          .map(([reason, count]) => `${REASONS[reason] ?? reason} (${count})`)
          .join(', ')
  return (
    <p className="mt-1 flex items-center gap-1.5 text-xs text-content-subtle">
      <Radar className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="truncate">
        Sombra {when}: nada entre {formatCount(shadow.releases)} — {reasons}
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
          Download {when} falhou{download.mensagem ? ` — ${download.mensagem}` : ''}
        </span>
      </p>
    )
  }
  if (download.estado === 'imported') {
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-success" title={download.release}>
        <CircleCheck className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">Importado — o Radarr adota o arquivo ao reler a pasta</span>
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
            Busca em todos os indexadores e escolhe com as regras do Radarr. Nada é baixado antes de você confirmar.
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

const hasProblem = (movie: Movie) => movie.arquivo !== null && movie.arquivo.disco !== 'ok'

function normalize(text: string) {
  return text.normalize('NFD').replace(/\p{Diacritic}/gu, '').toLowerCase()
}

function summarize(instances: MovieImport[]) {
  const failed = instances.find((instance) => instance.erro)
  if (failed) return { ok: false, text: `${failed.nome}: ${failed.erro}` }
  const total = instances.reduce(
    (sum, instance) => {
      const r = instance.resumo
      if (!r) return sum
      return {
        created: sum.created + r.created.length,
        updated: sum.updated + r.updated.length,
        removed: sum.removed + r.removed.length,
      }
    },
    { created: 0, updated: 0, removed: 0 },
  )
  if (total.created + total.updated + total.removed === 0) return { ok: true, text: 'Catálogo já estava em dia' }
  return {
    ok: true,
    text: `${total.created} novos, ${total.updated} atualizados, ${total.removed} removidos`,
  }
}

export function MoviesPage() {
  const queryClient = useQueryClient()
  const movies = useQuery({ queryKey: ['filmes'], queryFn: api.movies })
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('todos')

  const importer = useMutation({
    mutationFn: () => api.importMovies(true),
    onSuccess: ({ instancias }) => {
      if (instancias.length === 0) {
        toast.error('Nenhum gerenciador de filmes configurado em [[instances]]')
        return
      }
      const result = summarize(instancias)
      if (result.ok) toast.success(result.text)
      else toast.error(result.text)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
  })

  const shadow = useMutation({
    mutationFn: () => api.shadow(5),
    onSuccess: ({ filmes }) => toast.success(`Sombra: ${filmes.length} filmes buscados — veja abaixo`),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['filmes'] }),
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

  const visible = useMemo(() => {
    const needle = normalize(query.trim())
    return (list ?? []).filter((movie) => {
      if (filter === 'com-arquivo' && !movie.arquivo) return false
      if (filter === 'sem-arquivo' && movie.arquivo) return false
      if (filter === 'problemas' && !hasProblem(movie)) return false
      if (!needle) return true
      const haystack = normalize(
        [movie.titulo, movie.titulo_original ?? '', String(movie.ano ?? ''), movie.imdb ?? ''].join(' '),
      )
      return haystack.includes(needle)
    })
  }, [list, query, filter])

  return (
    <>
      <PageHeader
        title="Filmes"
        description="O catálogo do acervo-hub, espelhado do Radarr. O Radarr continua decidindo e baixando sozinho; a sombra mostra o que o acervo-hub pegaria, e “Pegar agora” faz o acervo-hub buscar, baixar e importar um filme que falta."
        action={
          <div className="flex flex-wrap gap-2">
            <Button onClick={() => shadow.mutate()} loading={shadow.isPending} disabled={counts.total === 0}>
              {!shadow.isPending && <Radar aria-hidden="true" />}
              {shadow.isPending ? 'Buscando em sombra…' : 'Rodar sombra'}
            </Button>
            <Button variant="primary" onClick={() => importer.mutate()} loading={importer.isPending}>
              {!importer.isPending && <CloudDownload aria-hidden="true" />}
              {importer.isPending ? 'Importando…' : 'Importar do Radarr'}
            </Button>
          </div>
        }
      />

      {movies.isPending ? (
        <MoviesSkeleton />
      ) : movies.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível ler o catálogo.</p>
          <p className="text-sm text-content-muted">{movies.error.message}</p>
          <Button onClick={() => void movies.refetch()}>Tentar novamente</Button>
        </div>
      ) : counts.total === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <Film className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">O catálogo está vazio</p>
          <p className="max-w-md text-sm text-content-muted">
            Importe os filmes do Radarr. Nada muda nele: o acervo-hub só lê a lista, os perfis e os arquivos.
          </p>
          <Button variant="primary" onClick={() => importer.mutate()} loading={importer.isPending}>
            Importar do Radarr
          </Button>
        </div>
      ) : (
        <>
          <dl className="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Stat label="Filmes" value={formatCount(counts.total)} />
            <Stat label="Com arquivo" value={formatCount(counts.withFile)} />
            <Stat label="No disco" value={formatSize(counts.size)} />
            <Stat
              label="Disco não confirma"
              value={formatCount(counts.problems)}
              tone={counts.problems > 0 ? 'danger' : undefined}
            />
          </dl>

          <div className="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <label className="sm:w-80">
              <span className="sr-only">Buscar no catálogo</span>
              <Input
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Título, título original, ano ou IMDb"
              />
            </label>
            <div role="radiogroup" aria-label="Filtro" className="flex flex-wrap rounded-md border border-border p-0.5">
              {FILTERS.map(({ value, label }) => (
                <button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={filter === value}
                  onClick={() => setFilter(value)}
                  className={cn(
                    'rounded-sm px-2.5 py-1.5 text-xs font-medium text-content-muted transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
                    filter === value && 'bg-surface-raised text-content',
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          {visible.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
              <SearchX className="size-6 text-content-subtle" aria-hidden="true" />
              <p className="font-medium">Nenhum filme com esse filtro</p>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setQuery('')
                  setFilter('todos')
                }}
              >
                Limpar filtros
              </Button>
            </div>
          ) : (
            <div className="overflow-hidden rounded-lg border border-border bg-surface">
              <table className="w-full text-sm">
                <caption className="sr-only">Filmes do catálogo</caption>
                <thead className="border-b border-border text-left text-xs text-content-subtle">
                  <tr>
                    <th scope="col" className="px-4 py-2.5 font-medium">
                      Filme
                    </th>
                    <th scope="col" className="hidden px-4 py-2.5 font-medium md:table-cell">
                      Qualidade
                    </th>
                    <th scope="col" className="hidden px-4 py-2.5 text-right font-medium sm:table-cell">
                      Tamanho
                    </th>
                    <th scope="col" className="px-4 py-2.5 text-right font-medium">
                      Disco
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {visible.map((movie) => (
                    <MovieRow key={movie.id} movie={movie} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <p className="mt-3 text-xs text-content-subtle tabular-nums">
            {formatCount(visible.length)} de {formatCount(counts.total)}
          </p>
        </>
      )}
    </>
  )
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: 'danger' }) {
  return (
    <div className="rounded-lg border border-border bg-surface px-4 py-3">
      <dt className="text-xs text-content-subtle">{label}</dt>
      <dd className={cn('mt-0.5 text-lg font-semibold tabular-nums', tone === 'danger' && 'text-danger')}>{value}</dd>
    </div>
  )
}

function MovieRow({ movie }: { movie: Movie }) {
  const [grabbing, setGrabbing] = useState(false)
  const file = movie.arquivo
  const disk = file ? DISK[file.disco] : null
  const imdb = movie.imdb ? `https://www.imdb.com/title/${movie.imdb}/` : undefined
  return (
    <tr className="align-top">
      {/* max-w-0 com w-full: é o que deixa o truncate funcionar dentro da tabela. */}
      <td className="w-full max-w-0 px-4 py-3">
        <p className="font-medium">
          {imdb ? (
            <a href={imdb} target="_blank" rel="noreferrer" className="hover:underline">
              {movie.titulo}
            </a>
          ) : (
            movie.titulo
          )}{' '}
          {movie.ano && <span className="font-normal text-content-subtle tabular-nums">({movie.ano})</span>}
        </p>
        {movie.titulo_original && movie.titulo_original !== movie.titulo && (
          <p className="text-xs text-content-subtle">{movie.titulo_original}</p>
        )}
        {file?.release && (
          <p className="mt-1 truncate font-mono text-xs text-content-subtle" title={file.release}>
            {file.release}
          </p>
        )}
        {!file && movie.download ? (
          <DownloadLine download={movie.download} />
        ) : (
          !file && movie.sombra && <ShadowLine shadow={movie.sombra} />
        )}
        {!file && movie.download?.estado !== 'downloading' && (
          <>
            <Button variant="ghost" size="sm" className="mt-2 -ml-2" onClick={() => setGrabbing(true)}>
              <ArrowDownToLine aria-hidden="true" />
              Pegar agora
            </Button>
            <GrabDialog movie={movie} open={grabbing} onOpenChange={setGrabbing} />
          </>
        )}
        {file && (
          <p className="mt-1 flex flex-wrap gap-1.5 md:hidden">
            <Badge>{file.qualidade}</Badge>
            <Badge className="sm:hidden">{formatSize(file.tamanho)}</Badge>
          </p>
        )}
      </td>
      <td className="hidden px-4 py-3 md:table-cell">
        {file ? (
          <div className="flex flex-col items-start gap-1">
            <Badge>{file.qualidade}</Badge>
            {file.idiomas.length > 0 && <span className="text-xs text-content-subtle">{file.idiomas.join(', ')}</span>}
          </div>
        ) : (
          <span className="text-content-subtle">—</span>
        )}
      </td>
      <td className="hidden px-4 py-3 text-right tabular-nums sm:table-cell">
        {file ? formatSize(file.tamanho) : <span className="text-content-subtle">—</span>}
      </td>
      <td className="px-4 py-3 text-right">
        {disk && file ? (
          <Tooltip content={file.disco_detalhe ?? file.nome}>
            <span className="inline-flex">
              <Badge tone={disk.tone}>
                <disk.icon aria-hidden="true" />
                <span className="hidden lg:inline">{disk.label}</span>
                <span className="sr-only lg:hidden">{disk.label}</span>
              </Badge>
            </span>
          </Tooltip>
        ) : (
          <Tooltip content={movie.monitorado ? 'Monitorado, esperando release' : 'Não monitorado'}>
            <span className="inline-flex">
              <Badge>
                {movie.monitorado ? <CircleDashed aria-hidden="true" /> : <HardDrive aria-hidden="true" />}
                <span className="hidden lg:inline">Sem arquivo</span>
                <span className="sr-only lg:hidden">Sem arquivo</span>
              </Badge>
            </span>
          </Tooltip>
        )}
      </td>
    </tr>
  )
}

function MoviesSkeleton() {
  return (
    <div aria-busy="true" aria-label="Carregando filmes">
      <div className="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-4">
        {[0, 1, 2, 3].map((key) => (
          <Skeleton key={key} className="h-[62px] rounded-lg" />
        ))}
      </div>
      <Skeleton className="h-96 rounded-lg" />
    </div>
  )
}
