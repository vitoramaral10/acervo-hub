import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Ban,
  ChevronRight,
  ExternalLink,
  ListFilter,
  LoaderCircle,
  Pencil,
  Radar,
  RotateCcw,
  Search,
  Trash2,
  X,
} from 'lucide-react'
import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { ConfirmButton } from '@/components/ConfirmButton'
import { HistoryList } from '@/components/HistoryList'
import { SeriesInteractiveSearch } from '@/components/SeriesInteractiveSearch'
import { SeriesPoster } from '@/components/SeriesPoster'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Badge, Checkbox, Skeleton, Switch, Tooltip } from '@/components/ui/misc'
import {
  type Episode,
  type Season,
  type SeriesChange,
  type SeriesDetail,
  type SeriesLastSearch,
  type SeriesSearchScope,
  seriesApi,
} from '@/lib/api'
import { formatAgo, formatCount, formatSize } from '@/lib/format'
import {
  SERIES_REASONS,
  distinctFiles,
  episodeCode,
  formatAirDate,
  seasonName,
  seriesStatus,
  skipLabel,
} from '@/lib/seriesFormat'
import { cn } from '@/lib/utils'

const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <dt className="text-xs text-content-subtle">{label}</dt>
      <dd className="mt-0.5 text-sm">{children}</dd>
    </div>
  )
}

function LastSearchLine({ search }: { search: SeriesLastSearch }) {
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
  const [first, ...others] = search.escolhidos
  if (first) {
    const rest = others.length
    return (
      <p className="mt-1 flex items-center gap-1.5 text-xs text-accent" title={first.titulo}>
        <Radar className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">
          Última busca {when}: escolheu <span className="font-mono">{first.titulo}</span>
          {rest > 0 && ` e mais ${rest}`}
        </span>
      </p>
    )
  }
  const reasons =
    search.releases === 0
      ? 'nenhum resultado'
      : search.motivos.map(([reason, count]) => `${SERIES_REASONS[reason] ?? reason} (${count})`).join(', ')
  return (
    <p className="mt-1 flex items-center gap-1.5 text-xs text-content-subtle">
      <Radar className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="truncate">
        Última busca {when}: nada entre {formatCount(search.releases)} — {reasons}
      </span>
    </p>
  )
}

function RemoveSeriesDialog({
  series,
  open,
  onOpenChange,
  onRemoved,
}: {
  series: SeriesDetail
  open: boolean
  onOpenChange: (open: boolean) => void
  onRemoved: () => void
}) {
  const queryClient = useQueryClient()
  const [deleteFiles, setDeleteFiles] = useState(true)
  const remove = useMutation({
    mutationFn: () => seriesApi.remove(series.id, { apagar_arquivos: deleteFiles }),
    onSuccess: () => {
      toast.success(`${series.titulo} removida`)
      onRemoved()
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ['series'] })
      void queryClient.invalidateQueries({ queryKey: ['fila'] })
    },
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Remover {series.titulo}?</DialogTitle>
          <DialogDescription>A série sai da biblioteca.</DialogDescription>
        </DialogHeader>
        <div className="flex items-start justify-between gap-4">
          <div>
            <Label htmlFor="remover-serie-arquivos">Apagar os arquivos e os downloads</Label>
            <p className="text-xs text-content-subtle">
              A pasta da série e os torrents no qBittorrent, com os dados, na hora.
            </p>
            <p className="font-mono text-xs break-all text-content-subtle">{series.pasta}</p>
          </div>
          <Switch id="remover-serie-arquivos" checked={deleteFiles} onCheckedChange={setDeleteFiles} />
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

function SeriesSettings({ series }: { series: SeriesDetail }) {
  const queryClient = useQueryClient()
  const edit = useMutation({
    mutationFn: (change: SeriesChange) => seriesApi.edit(series.id, change),
    onSuccess: () => toast.success('Série atualizada'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['series'] }),
  })
  return (
    <div className="grid gap-3 rounded-md border border-border p-4 sm:grid-cols-2">
      <div className="flex items-start gap-2.5">
        <Checkbox
          id={`monitorar-${series.id}`}
          className="mt-0.5"
          checked={series.monitorar_novos}
          disabled={edit.isPending}
          onChange={(event) => edit.mutate({ monitorar_novos: event.target.checked })}
        />
        <div>
          <Label htmlFor={`monitorar-${series.id}`} className="font-normal">
            Episódios novos entram como Quero
          </Label>
          <p className="text-xs text-content-subtle">Desligado, o que o TMDB anunciar entra como “não quero”.</p>
        </div>
      </div>
      <div className="flex items-start gap-2.5">
        <Checkbox
          id={`pasta-temporada-${series.id}`}
          className="mt-0.5"
          checked={series.pasta_de_temporada}
          disabled={edit.isPending}
          onChange={(event) => edit.mutate({ pasta_de_temporada: event.target.checked })}
        />
        <div>
          <Label htmlFor={`pasta-temporada-${series.id}`} className="font-normal">
            Pasta por temporada
          </Label>
          <p className="text-xs text-content-subtle">Vale para os próximos arquivos importados.</p>
        </div>
      </div>
    </div>
  )
}

function SeriesHistory({ id }: { id: number }) {
  const history = useQuery({ queryKey: ['series', id, 'historico'], queryFn: () => seriesApi.history(id) })
  if (history.isPending) return <Skeleton className="h-20" />
  if (history.isError)
    return (
      <p role="alert" className="text-sm text-danger">
        {history.error.message}
      </p>
    )
  if (history.data.eventos.length === 0) return <p className="text-sm text-content-subtle">Nada registrado ainda.</p>
  return <HistoryList events={history.data.eventos} showMovie />
}

// ---------------------------------------------------------------- episódios

/** O estado do episódio numa palavra, com o que importa de cada um. */
function StateChip({ episode }: { episode: Episode }) {
  if (episode.estado === 'tenho') {
    const file = episode.arquivo
    return (
      <Badge tone="success" title={file?.release ?? file?.nome}>
        Tenho
        {file && (
          <span className="font-normal tabular-nums">
            · {file.qualidade} · {formatSize(file.tamanho)}
          </span>
        )}
      </Badge>
    )
  }
  if (episode.estado === 'dispensado') return <Badge>{skipLabel(episode)}</Badge>
  return <Badge tone="warning">Quero</Badge>
}

function IconAction({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string
  onClick: () => void
  disabled?: boolean
  children: ReactNode
}) {
  return (
    <Tooltip content={label}>
      <Button variant="ghost" size="icon-sm" aria-label={label} disabled={disabled} onClick={onClick}>
        {children}
      </Button>
    </Tooltip>
  )
}

function EpisodeRow({
  season,
  episode,
  sharedWith,
  selected,
  busy,
  onSelect,
  onDelete,
  onSkip,
  onSearch,
}: {
  season: number
  episode: Episode
  /** Quantos outros episódios dividem o mesmo arquivo. */
  sharedWith: number
  selected: boolean
  busy: boolean
  onSelect: (on: boolean) => void
  onDelete: () => void
  onSkip: (skip: 'unwanted' | null) => void
  onSearch: () => void
}) {
  const [confirming, setConfirming] = useState(false)
  const code = episodeCode(season, episode.numero)
  const aired = episode.exibido
  return (
    <li
      className={cn(
        'flex flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-2 sm:flex-nowrap',
        selected && 'bg-accent-soft/40',
      )}
    >
      <Checkbox checked={selected} onChange={(event) => onSelect(event.target.checked)} aria-label={`Selecionar ${code}`} />
      <span className={cn('w-8 shrink-0 text-right text-sm text-content-subtle tabular-nums', !aired && 'opacity-60')}>
        {episode.numero}
      </span>
      <div className={cn('min-w-0 flex-1 basis-40', !aired && 'opacity-60')}>
        <p className="truncate text-sm" title={episode.titulo ?? undefined}>
          {episode.titulo || 'Sem título'}
        </p>
        <p className="text-xs text-content-subtle tabular-nums">
          {aired ? formatAirDate(episode.data) : `Estreia ${formatAirDate(episode.data)}`}
        </p>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        <StateChip episode={episode} />
        {episode.baixando && (
          <span title="Baixando">
            <LoaderCircle
              className="size-4 animate-spin text-accent motion-reduce:animate-none"
              aria-label="Baixando"
            />
          </span>
        )}
      </div>
      {confirming ? (
        <div className="ml-auto flex shrink-0 flex-wrap items-center justify-end gap-1.5">
          <span className="text-xs text-content-muted">
            {sharedWith > 0 ? `O arquivo tem mais ${sharedWith} ep. e sai inteiro.` : 'Apagar o arquivo?'}
          </span>
          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              setConfirming(false)
              onDelete()
            }}
          >
            <Trash2 aria-hidden="true" />
            Apagar
          </Button>
          <Button size="sm" variant="ghost" autoFocus onClick={() => setConfirming(false)}>
            Cancelar
          </Button>
        </div>
      ) : (
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          {episode.arquivo && (
            <IconAction label={`Apagar ${code}`} disabled={busy} onClick={() => setConfirming(true)}>
              <Trash2 className="text-danger" aria-hidden="true" />
            </IconAction>
          )}
          <IconAction label={`Buscar ${code}`} disabled={busy} onClick={onSearch}>
            <Search aria-hidden="true" />
          </IconAction>
          {episode.estado === 'dispensado' ? (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => onSkip(null)} aria-label={`Quero ${code} de novo`}>
              <RotateCcw aria-hidden="true" />
              <span className="hidden sm:inline">Quero de novo</span>
            </Button>
          ) : episode.estado === 'quero' ? (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => onSkip('unwanted')} aria-label={`Não quero ${code}`}>
              <Ban aria-hidden="true" />
              <span className="hidden sm:inline">Não quero</span>
            </Button>
          ) : null}
        </div>
      )}
    </li>
  )
}

// ---------------------------------------------------------------- temporadas

function SeasonSection({
  season,
  open,
  onToggle,
  selection,
  sharedFiles,
  pending,
  onSelect,
  onDelete,
  onSkip,
  onSearch,
}: {
  season: Season
  open: boolean
  onToggle: () => void
  selection: Set<number>
  sharedFiles: Map<number, number>
  pending: Set<number>
  onSelect: (ids: number[], on: boolean) => void
  onDelete: (ids: number[]) => void
  onSkip: (ids: number[], skip: 'unwanted' | null) => void
  onSearch: (scope: SeriesSearchScope, label: string) => void
}) {
  const { totais, episodios } = season
  const name = seasonName(season.numero)
  const files = distinctFiles(episodios)
  const wanted = episodios.filter((e) => e.estado === 'quero').map((e) => e.id)
  const dismissed = episodios.filter((e) => e.estado === 'dispensado')
  const regrets = dismissed.filter((e) => e.motivo !== 'unwanted').length
  const picked = episodios.filter((e) => selection.has(e.id)).length
  const all = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (all.current) all.current.indeterminate = picked > 0 && picked < episodios.length
  }, [picked, episodios.length])
  const busy = episodios.some((e) => pending.has(e.id))

  return (
    <section className="rounded-lg border border-border">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 bg-surface-raised/50 px-3 py-2">
        <Checkbox
          ref={all}
          checked={picked > 0 && picked === episodios.length}
          onChange={(event) => onSelect(episodios.map((e) => e.id), event.target.checked)}
          aria-label={`Selecionar toda a ${name.toLowerCase()}`}
        />
        <button
          type="button"
          aria-expanded={open}
          onClick={onToggle}
          className="flex min-w-0 flex-1 basis-56 items-center gap-1.5 rounded-sm text-left focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          <ChevronRight
            className={cn('size-4 shrink-0 transition-transform motion-reduce:transition-none', open && 'rotate-90')}
            aria-hidden="true"
          />
          <span className="font-medium">{name}</span>
          <span className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-content-muted tabular-nums">
            <span className={totais.quero > 0 ? 'text-warning' : undefined}>Quero {totais.quero}</span>
            <span className={totais.tenho > 0 ? 'text-success' : undefined}>Tenho {totais.tenho}</span>
            <span>Dispensado {totais.dispensado}</span>
            {totais.baixando > 0 && (
              <span className="inline-flex items-center gap-1 text-accent">
                <LoaderCircle className="size-3 animate-spin motion-reduce:animate-none" aria-hidden="true" />
                {totais.baixando} baixando
              </span>
            )}
          </span>
        </button>
        <div className="flex flex-wrap items-center gap-1">
          {files > 0 && (
            <ConfirmButton
              size="sm"
              variant="ghost"
              disabled={busy}
              title={`Apagar ${name.toLowerCase()}?`}
              description={`${plural(files, 'arquivo sai', 'arquivos saem')} do disco, e os torrents que ficarem sem nenhum arquivo em uso saem do qBittorrent. Os episódios ficam como “apagado” e não são buscados de novo.`}
              confirmLabel="Apagar temporada"
              onConfirm={() => onDelete(episodios.filter((e) => e.arquivo).map((e) => e.id))}
            >
              <Trash2 className="text-danger" aria-hidden="true" />
              Apagar temporada
            </ConfirmButton>
          )}
          <Button
            size="sm"
            variant="ghost"
            disabled={busy || wanted.length === 0}
            onClick={() => onSkip(wanted, 'unwanted')}
          >
            <Ban aria-hidden="true" />
            Não quero a temporada
          </Button>
          {regrets > 0 ? (
            <ConfirmButton
              size="sm"
              variant="ghost"
              disabled={busy}
              title={`Quero a ${name.toLowerCase()}?`}
              description={`${plural(dismissed.length, 'episódio dispensado volta', 'episódios dispensados voltam')} a ser buscado, incluindo ${plural(regrets, 'apagado ou assistido', 'apagados ou assistidos')}.`}
              confirmLabel="Quero"
              onConfirm={() => onSkip(dismissed.map((e) => e.id), null)}
            >
              <RotateCcw aria-hidden="true" />
              Quero a temporada
            </ConfirmButton>
          ) : (
            <Button
              size="sm"
              variant="ghost"
              disabled={busy || dismissed.length === 0}
              onClick={() => onSkip(dismissed.map((e) => e.id), null)}
            >
              <RotateCcw aria-hidden="true" />
              Quero a temporada
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => onSearch({ temporada: season.numero }, name)}>
            <Search aria-hidden="true" />
            Buscar temporada
          </Button>
        </div>
      </div>
      {open && (
        <ul className="divide-y divide-border border-t border-border">
          {episodios.map((episode) => (
            <EpisodeRow
              key={episode.id}
              season={season.numero}
              episode={episode}
              sharedWith={episode.arquivo ? (sharedFiles.get(episode.arquivo.id) ?? 1) - 1 : 0}
              selected={selection.has(episode.id)}
              busy={pending.has(episode.id)}
              onSelect={(on) => onSelect([episode.id], on)}
              onDelete={() => onDelete([episode.id])}
              onSkip={(skip) => onSkip([episode.id], skip)}
              onSearch={() =>
                onSearch({ episodios: [episode.id] }, episodeCode(season.numero, episode.numero))
              }
            />
          ))}
        </ul>
      )}
    </section>
  )
}

/** Seleção múltipla: some quando vazia e fica à vista enquanto se rola a lista. */
function SelectionBar({
  episodes,
  busy,
  onClear,
  onDelete,
  onSkip,
}: {
  episodes: Episode[]
  busy: boolean
  onClear: () => void
  onDelete: (ids: number[]) => void
  onSkip: (ids: number[], skip: 'unwanted' | null) => void
}) {
  const withFile = episodes.filter((e) => e.arquivo)
  const wanted = episodes.filter((e) => e.estado === 'quero')
  const dismissed = episodes.filter((e) => e.estado === 'dispensado')
  const files = distinctFiles(episodes)
  return (
    <div
      role="region"
      aria-label="Ações da seleção"
      className="sticky bottom-0 z-10 flex flex-wrap items-center gap-2 border-t border-border bg-surface px-6 py-3 shadow-[0_-4px_12px_rgb(0_0_0/0.08)]"
    >
      <p className="mr-auto text-sm font-medium tabular-nums">{plural(episodes.length, 'selecionado', 'selecionados')}</p>
      <ConfirmButton
        size="sm"
        variant="secondary"
        disabled={busy || withFile.length === 0}
        title="Apagar os selecionados?"
        description={`${plural(files, 'arquivo sai', 'arquivos saem')} do disco. Os episódios ficam como “apagado” e não são buscados de novo.`}
        confirmLabel="Apagar selecionados"
        onConfirm={() => onDelete(withFile.map((e) => e.id))}
      >
        <Trash2 className="text-danger" aria-hidden="true" />
        Apagar selecionados
      </ConfirmButton>
      <Button
        size="sm"
        disabled={busy || wanted.length === 0}
        onClick={() => onSkip(wanted.map((e) => e.id), 'unwanted')}
      >
        <Ban aria-hidden="true" />
        Não quero
      </Button>
      <Button
        size="sm"
        disabled={busy || dismissed.length === 0}
        onClick={() => onSkip(dismissed.map((e) => e.id), null)}
      >
        <RotateCcw aria-hidden="true" />
        Quero
      </Button>
      <Button size="sm" variant="ghost" onClick={onClear}>
        <X aria-hidden="true" />
        Limpar
      </Button>
    </div>
  )
}

// ---------------------------------------------------------------- detalhe

function SeriesBody({ series, onClose }: { series: SeriesDetail; onClose: () => void }) {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [showHistory, setShowHistory] = useState(false)
  const [search, setSearch] = useState<{ scope: SeriesSearchScope; label: string } | null>(null)
  const [selection, setSelection] = useState<Set<number>>(new Set())

  // Do mais novo ao mais velho, com os especiais por último; a mais recente abre sozinha.
  const seasons = useMemo(
    () => [...series.temporadas].sort((a, b) => (a.numero === 0 ? 1 : b.numero === 0 ? -1 : b.numero - a.numero)),
    [series.temporadas],
  )
  const [openSeasons, setOpenSeasons] = useState<Set<number>>(() => new Set(seasons.slice(0, 1).map((s) => s.numero)))

  const episodes = useMemo(() => series.temporadas.flatMap((s) => s.episodios), [series.temporadas])
  const sharedFiles = useMemo(() => {
    const counts = new Map<number, number>()
    for (const episode of episodes) {
      if (episode.arquivo) counts.set(episode.arquivo.id, (counts.get(episode.arquivo.id) ?? 0) + 1)
    }
    return counts
  }, [episodes])
  const selected = useMemo(() => episodes.filter((e) => selection.has(e.id)), [episodes, selection])

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ['series'] })
    void queryClient.invalidateQueries({ queryKey: ['fila'] })
  }
  const unselect = (ids: number[]) =>
    setSelection((old) => {
      const next = new Set(old)
      for (const id of ids) next.delete(id)
      return next
    })

  const searchNow = useMutation({
    mutationFn: () => seriesApi.searchNow(series.id),
    onSuccess: ({ iniciada }) =>
      iniciada ? toast.success('Busca iniciada') : toast.info('Já há uma busca desta série em andamento'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: refresh,
  })
  const del = useMutation({
    mutationFn: (ids: number[]) => seriesApi.deleteEpisodes(series.id, ids),
    onSuccess: (result, ids) => {
      toast.success(
        `${plural(result.arquivos, 'arquivo apagado', 'arquivos apagados')}` +
          (result.torrents > 0 ? ` e ${plural(result.torrents, 'torrent removido', 'torrents removidos')}` : ''),
      )
      if (result.aviso) toast.warning(result.aviso)
      unselect(ids)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: refresh,
  })
  const skip = useMutation({
    mutationFn: ({ ids, skip: value }: { ids: number[]; skip: 'unwanted' | null }) =>
      seriesApi.skipEpisodes(series.id, ids, value),
    onSuccess: ({ alterados }, { skip: value, ids }) => {
      toast.success(
        value === null
          ? `${plural(alterados, 'episódio volta', 'episódios voltam')} a ser buscado${alterados === 1 ? '' : 's'}`
          : `${plural(alterados, 'episódio dispensado', 'episódios dispensados')}`,
      )
      unselect(ids)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: refresh,
  })

  const pending = useMemo(() => {
    const ids = new Set<number>()
    if (del.isPending) for (const id of del.variables) ids.add(id)
    if (skip.isPending) for (const id of skip.variables.ids) ids.add(id)
    return ids
  }, [del.isPending, del.variables, skip.isPending, skip.variables])

  const imdb = series.imdb ? `https://www.imdb.com/title/${series.imdb}/` : null
  const totals = series.episodios
  const onSkip = (ids: number[], value: 'unwanted' | null) => skip.mutate({ ids, skip: value })

  return (
    <>
      <div className="grid gap-6 p-6 sm:grid-cols-[12rem_1fr]">
        <SeriesPoster poster={series.poster} title={series.titulo} className="mx-auto w-40 sm:w-full" />
        <div className="flex min-w-0 flex-col gap-4">
          <DialogHeader>
            <DialogTitle className="text-2xl">
              {series.titulo}{' '}
              {series.ano && <span className="font-normal text-content-subtle tabular-nums">({series.ano})</span>}
            </DialogTitle>
            {series.titulo_original && series.titulo_original !== series.titulo && (
              <p className="text-sm text-content-subtle">{series.titulo_original}</p>
            )}
          </DialogHeader>
          <DialogDescription className="max-w-[65ch] leading-relaxed">
            {series.sinopse || 'Sem sinopse.'}
          </DialogDescription>
          <dl className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-4">
            {series.rede && <Fact label="Rede">{series.rede}</Fact>}
            {series.status && <Fact label="Status">{seriesStatus(series.status)}</Fact>}
            <Fact label="Episódios">
              <span className="tabular-nums">
                {totals.tenho} no disco
                {totals.quero_exibidos > 0 && <span className="text-warning"> · {totals.quero_exibidos} faltam</span>}
              </span>
            </Fact>
            <Fact label="Tamanho">
              <span className="tabular-nums">{formatSize(series.tamanho)}</span>
            </Fact>
          </dl>

          {(series.downloads.length > 0 || series.ultima_busca) && (
            <div className="border-t border-border pt-3">
              {series.downloads.length > 0 && (
                <ul className="grid gap-1.5">
                  {series.downloads.map((download) => (
                    <li
                      key={download.id}
                      className="flex items-center gap-1.5 text-xs text-accent"
                      title={download.release}
                    >
                      <LoaderCircle
                        className="size-3.5 shrink-0 animate-spin motion-reduce:animate-none"
                        aria-hidden="true"
                      />
                      <span className="truncate">
                        Baixando {download.episodios}
                        {download.mensagem ? ` (${download.mensagem})` : ''}:{' '}
                        <span className="font-mono">{download.release}</span>
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {series.ultima_busca && <LastSearchLine search={series.ultima_busca} />}
            </div>
          )}

          <div className="flex flex-wrap gap-2">
            <Button variant="primary" loading={searchNow.isPending} onClick={() => searchNow.mutate()}>
              {!searchNow.isPending && <Radar aria-hidden="true" />}
              Buscar agora
            </Button>
            <Button onClick={() => setSearch({ scope: {}, label: series.titulo })}>
              <ListFilter aria-hidden="true" />
              Busca interativa
            </Button>
            <Button variant="ghost" aria-expanded={editing} onClick={() => setEditing((v) => !v)}>
              <Pencil aria-hidden="true" />
              Editar
            </Button>
            <Button variant="ghost" onClick={() => setRemoving(true)}>
              <Trash2 aria-hidden="true" />
              Remover
            </Button>
            {imdb && (
              <Button asChild variant="ghost">
                <a href={imdb} target="_blank" rel="noreferrer">
                  IMDb
                  <ExternalLink aria-hidden="true" />
                </a>
              </Button>
            )}
          </div>
        </div>

        <div className="grid gap-4 sm:col-span-2">
          {editing && <SeriesSettings series={series} />}

          {seasons.length === 0 ? (
            <p className="rounded-lg border border-dashed border-border-strong px-6 py-10 text-center text-sm text-content-muted">
              O TMDB ainda não trouxe nenhum episódio desta série.
            </p>
          ) : (
            <div className="grid gap-3">
              {seasons.map((season) => (
                <SeasonSection
                  key={season.numero}
                  season={season}
                  open={openSeasons.has(season.numero)}
                  onToggle={() =>
                    setOpenSeasons((old) => {
                      const next = new Set(old)
                      if (!next.delete(season.numero)) next.add(season.numero)
                      return next
                    })
                  }
                  selection={selection}
                  sharedFiles={sharedFiles}
                  pending={pending}
                  onSelect={(ids, on) =>
                    setSelection((old) => {
                      const next = new Set(old)
                      for (const id of ids) {
                        if (on) next.add(id)
                        else next.delete(id)
                      }
                      return next
                    })
                  }
                  onDelete={(ids) => del.mutate(ids)}
                  onSkip={onSkip}
                  onSearch={(scope, label) => setSearch({ scope, label: `${series.titulo} · ${label}` })}
                />
              ))}
            </div>
          )}

          <div>
            <button
              type="button"
              aria-expanded={showHistory}
              onClick={() => setShowHistory((v) => !v)}
              className="flex items-center gap-1.5 rounded-sm text-sm font-medium text-content-muted hover:text-content focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
            >
              <ChevronRight
                className={cn('size-4 transition-transform motion-reduce:transition-none', showHistory && 'rotate-90')}
                aria-hidden="true"
              />
              Histórico da série
            </button>
            {showHistory && (
              <div className="mt-2">
                <SeriesHistory id={series.id} />
              </div>
            )}
          </div>
        </div>
      </div>

      {selected.length > 0 && (
        <SelectionBar
          episodes={selected}
          busy={del.isPending || skip.isPending}
          onClear={() => setSelection(new Set())}
          onDelete={(ids) => del.mutate(ids)}
          onSkip={onSkip}
        />
      )}

      {search && (
        <SeriesInteractiveSearch
          seriesId={series.id}
          title={search.label}
          scope={search.scope}
          onClose={() => setSearch(null)}
        />
      )}
      <RemoveSeriesDialog
        series={series}
        open={removing}
        onOpenChange={setRemoving}
        onRemoved={() => {
          setRemoving(false)
          onClose()
        }}
      />
    </>
  )
}

/** Painel de uma série: cabeçalho, ações, temporadas em acordeão e histórico. */
export function SeriesDetails({ id, onClose }: { id: number; onClose: () => void }) {
  const detail = useQuery({
    queryKey: ['series', id],
    queryFn: () => seriesApi.get(id),
    // Enquanto baixa, a tela acompanha o que o servidor importa.
    refetchInterval: (state) => ((state.state.data?.episodios.baixando ?? 0) > 0 ? 5_000 : false),
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-4xl gap-0 p-0">
        {detail.isPending ? (
          <div className="grid gap-4 p-6" aria-busy="true">
            <DialogHeader>
              <DialogTitle>Carregando série…</DialogTitle>
              <DialogDescription className="sr-only">Lendo a série e os episódios.</DialogDescription>
            </DialogHeader>
            <Skeleton className="h-40" />
            <Skeleton className="h-24" />
          </div>
        ) : detail.isError ? (
          <div role="alert" className="grid gap-3 p-6">
            <DialogHeader>
              <DialogTitle>Não foi possível ler a série</DialogTitle>
              <DialogDescription>{detail.error.message}</DialogDescription>
            </DialogHeader>
            <div>
              <Button onClick={() => void detail.refetch()}>Tentar novamente</Button>
            </div>
          </div>
        ) : (
          <SeriesBody series={detail.data} onClose={onClose} />
        )}
        {detail.isFetching && !detail.isPending && <span className="sr-only">Atualizando…</span>}
      </DialogContent>
    </Dialog>
  )
}

